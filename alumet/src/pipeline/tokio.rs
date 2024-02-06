use std::{
    future::Future,
    io,
    pin::Pin,
    time::{Duration, Instant, SystemTime},
};

use crate::{
    metrics::MeasurementBuffer,
    pipeline::{Output, Source, Transform},
};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio::{runtime::Runtime, sync::watch};

use super::registry::MetricRegistry;
use super::{
    threading, PollError, PollErrorKind, TransformError, TransformErrorKind, WriteError,
};
use tokio_stream::StreamExt;

pub struct TaggedTransform {
    transform: Box<dyn Transform>,
    plugin_name: String,
}
pub struct TaggedOutput {
    output: Box<dyn Output>,
    plugin_name: String,
}
pub struct TaggedSource {
    source: Box<dyn Source>,
    source_type: SourceType,
    trigger_provider: SourceTriggerProvider,
    plugin_name: String,
}
/// A boxed future, from the `futures` crate.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Debug)]
pub enum SourceTriggerProvider {
    TimeInterval {
        start_time: Instant,
        poll_interval: Duration,
        flush_interval: Duration,
    },
    Future {
        f: fn() -> BoxFuture<'static, SourceTriggerOutput>,
        flush_rounds: usize,
    },
}
impl SourceTriggerProvider {
    pub fn provide(self) -> io::Result<(SourceTrigger, usize)> {
        match self {
            SourceTriggerProvider::TimeInterval {
                start_time,
                poll_interval,
                flush_interval,
            } => {
                let flush_rounds = (flush_interval.as_micros() / poll_interval.as_micros()) as usize;
                let trigger = SourceTrigger::TimeInterval(tokio_timerfd::Interval::new(start_time, poll_interval)?);
                Ok((trigger, flush_rounds))
            }
            SourceTriggerProvider::Future { f, flush_rounds } => {
                let trigger = SourceTrigger::Future(f);
                Ok((trigger, flush_rounds))
            }
        }
    }
}

pub type SourceTriggerOutput = Result<(), PollError>;
pub enum SourceTrigger {
    TimeInterval(tokio_timerfd::Interval),
    Future(fn() -> BoxFuture<'static, SourceTriggerOutput>),
}

impl TaggedSource {
    pub fn new(
        source: Box<dyn Source>,
        source_type: SourceType,
        trigger_provider: SourceTriggerProvider,
        plugin_name: String,
    ) -> TaggedSource {
        TaggedSource {
            source,
            source_type,
            trigger_provider,
            plugin_name,
        }
    }
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceType {
    Normal,
    // Blocking, // todo: how to provide this type properly?
    RealtimePriority,
}

struct PipelineElements {
    sources: Vec<TaggedSource>,
    transforms: Vec<Box<dyn Transform>>,
    outputs: Vec<Box<dyn Output>>,
}

struct PipelineParameters {
    normal_worker_threads: Option<usize>,
    priority_worker_threads: Option<usize>,
}

impl PipelineParameters {
    fn build_normal_runtime(&self) -> io::Result<tokio::runtime::Runtime> {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.enable_all().thread_name("normal-worker");
        if let Some(n) = self.normal_worker_threads {
            builder.worker_threads(n);
        }
        builder.build()
    }

    fn build_priority_runtime(&self) -> io::Result<tokio::runtime::Runtime> {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder
            .enable_all()
            .on_thread_start(|| {
                threading::increase_thread_priority().expect("failed to create high-priority thread for worker")
            })
            .thread_name("priority-worker");
        if let Some(n) = self.priority_worker_threads {
            builder.worker_threads(n);
        }
        builder.build()
    }
}

/// A builder for measurement pipelines.
pub struct PendingPipeline {
    elements: PipelineElements,
    params: PipelineParameters,
}

pub struct PipelineController {
    // Keep the tokio runtimes alive
    normal_runtime: Runtime,
    priority_runtime: Option<Runtime>,

    // Handles to wait for sources to finish.
    source_handles: Vec<JoinHandle<Result<(), PollError>>>,
    output_handles: Vec<JoinHandle<Result<(), WriteError>>>,
    transform_handle: JoinHandle<Result<(), TransformError>>,

    // Senders to keep the receivers alive and to send commands.
    source_command_senders: Vec<watch::Sender<SourceCmd>>,
    output_command_senders: Vec<watch::Sender<OutputCmd>>,
}
impl PipelineController {
    /// Blocks the current thread until all tasks in the pipeline finish.
    pub fn wait_for_all(&mut self) {
        self.normal_runtime.block_on(async {
            for handle in &mut self.source_handles {
                handle.await.unwrap().unwrap();// todo: handle errors
            }

            (&mut self.transform_handle).await.unwrap().unwrap();

            for handle in &mut self.output_handles {
                handle.await.unwrap().unwrap();
            }
        });
    }

    pub fn command_all_sources(&self, command: SourceCmd) {
        for sender in &self.source_command_senders {
            sender.send(command.clone()).unwrap();
        }
    }

    pub fn command_all_outputs(&self, command: OutputCmd) {
        for sender in &self.output_command_senders {
            sender.send(command.clone()).unwrap();
        }
    }
}

impl PendingPipeline {
    pub fn new(sources: Vec<TaggedSource>, transforms: Vec<Box<dyn Transform>>, outputs: Vec<Box<dyn Output>>) -> Self {
        PendingPipeline {
            elements: PipelineElements {
                sources,
                transforms: transforms,
                outputs: outputs,
            },
            params: PipelineParameters {
                normal_worker_threads: None,
                priority_worker_threads: None,
            },
        }
    }
    pub fn normal_worker_threads(&mut self, n: usize) {
        self.params.normal_worker_threads = Some(n);
    }
    pub fn priority_worker_threads(&mut self, n: usize) {
        self.params.priority_worker_threads = Some(n);
    }

    pub fn start(self, metrics: MetricRegistry) -> PipelineController {
        // set the global metric registry, which can be accessed by the pipeline's elements (sources, transforms, outputs)
        MetricRegistry::init_global(metrics);

        // Create the runtimes
        let normal_runtime: Runtime = self.params.build_normal_runtime().unwrap();

        let priority_runtime: Option<Runtime> = {
            let mut res = None;
            for src in &self.elements.sources {
                if src.source_type == SourceType::RealtimePriority {
                    res = Some(self.params.build_priority_runtime().unwrap());
                    break;
                }
            }
            res
        };

        // Channel sources -> transforms
        let (in_tx, in_rx) = mpsc::channel::<MeasurementBuffer>(256);

        // if self.elements.transforms.is_empty() && self.elements.outputs.len() == 1 {
        // TODO: If no transforms and one output, the pipeline can be reduced
        // }

        // Broadcast queue transforms -> outputs
        let out_tx = broadcast::Sender::<MeasurementBuffer>::new(256);

        // Store the task handles in order to wait for them to complete before stopping,
        // and the command senders in order to keep the receivers alive and to be able to send commands after the launch.
        let mut source_handles = Vec::with_capacity(self.elements.sources.len());
        let mut output_handles = Vec::with_capacity(self.elements.outputs.len());
        let mut source_command_senders = Vec::with_capacity(self.elements.sources.len());
        let mut output_command_senders = Vec::with_capacity(self.elements.outputs.len());

        // Start the tasks, starting at the end of the pipeline (to avoid filling the buffers too quickly).
        // 1. Outputs
        for out in self.elements.outputs {
            let data_rx = out_tx.subscribe();
            let (command_tx, command_rx) = watch::channel(OutputCmd::Run);
            let handle = normal_runtime.spawn(run_output_from_broadcast(out, data_rx, command_rx));
            output_handles.push(handle);
            output_command_senders.push(command_tx);
        }

        // 2. Transforms
        let transform_handle = normal_runtime.spawn(run_transforms(self.elements.transforms, in_rx, out_tx));

        // 3. Sources
        for src in self.elements.sources {
            let data_tx = in_tx.clone();
            let (command_tx, command_rx) = watch::channel(SourceCmd::SetTrigger(Some(src.trigger_provider)));
            let runtime = match src.source_type {
                SourceType::Normal => &normal_runtime,
                SourceType::RealtimePriority => priority_runtime.as_ref().unwrap(),
            };
            let handle = runtime.spawn(run_source(src.source, data_tx, command_rx));
            source_handles.push(handle);
            source_command_senders.push(command_tx);
        }

        PipelineController {
            normal_runtime,
            priority_runtime,
            source_handles,
            output_handles,
            transform_handle,
            source_command_senders,
            output_command_senders,
        }
    }
}

#[derive(Clone, Debug)]
pub enum SourceCmd {
    Run,
    Pause,
    Stop,
    SetTrigger(Option<SourceTriggerProvider>),
}

async fn run_source(
    mut source: Box<dyn Source>,
    tx: mpsc::Sender<MeasurementBuffer>,
    mut commands: watch::Receiver<SourceCmd>,
) -> Result<(), PollError> {
    fn init_trigger(provider: &mut Option<SourceTriggerProvider>) -> Result<(SourceTrigger, usize), PollError> {
        provider
            .take()
            .expect("invalid empty trigger in message Init(trigger)")
            .provide()
            .map_err(|e| {
                PollError::with_source(PollErrorKind::Unrecoverable, "Source trigger initialization failed", e)
            })
    }

    // the first command must be "init"
    let (mut trigger, mut flush_rounds) = {
        let init_cmd = commands
            .wait_for(|c| matches!(c, SourceCmd::SetTrigger(_)))
            .await
            .map_err(|e| {
                PollError::with_source(PollErrorKind::Unrecoverable, "Source task initialization failed", e)
            })?;

        match (*init_cmd).clone() {
            // cloning required to borrow opt as mut below
            SourceCmd::SetTrigger(mut opt) => init_trigger(&mut opt)?,
            _ => unreachable!(),
        }
    };

    // main loop
    let mut buffer = MeasurementBuffer::new();
    let mut i = 1usize; // start at 1 to avoid flushing right away
    'run: loop {
        if i % flush_rounds == 0 {
            // flush and update the command, not on every round for performance reasons
            // flush
            tx.try_send(buffer).expect("todo: handle failed send (source too fast)");
            buffer = MeasurementBuffer::new();

            // update state based on the latest command
            if commands.has_changed().unwrap() {
                let mut paused = false;
                'pause: loop {
                    let cmd = if paused {
                        commands
                            .changed()
                            .await
                            .expect("The output channel of paused source should be open.");
                        (*commands.borrow()).clone()
                    } else {
                        (*commands.borrow_and_update()).clone()
                    };
                    println!("Source COMMAND has changed: {cmd:?}");
                    match cmd {
                        SourceCmd::Run => break 'pause,
                        SourceCmd::Pause => paused = true,
                        SourceCmd::Stop => break 'run,
                        SourceCmd::SetTrigger(mut opt) => {
                            (trigger, flush_rounds) = init_trigger(&mut opt)?;
                            if !paused {
                                break 'pause;
                            }
                        }
                    }
                }
            }
        }
        i += 1;

        // wait for trigger
        match trigger {
            SourceTrigger::TimeInterval(ref mut interval) => {
                interval.next().await.unwrap().unwrap();
            }
            SourceTrigger::Future(f) => {
                f().await?;
            }
        };

        // poll the source
        let timestamp = SystemTime::now();
        source.poll(&mut buffer.as_accumulator(), timestamp);
    }
    Ok(())
}

async fn run_transforms(
    mut transforms: Vec<Box<dyn Transform>>,
    mut rx: mpsc::Receiver<MeasurementBuffer>,
    tx: broadcast::Sender<MeasurementBuffer>,
) -> Result<(), TransformError> {
    loop {
        if let Some(mut measurements) = rx.recv().await {
            for t in &mut transforms {
                // if one transform fails, we cannot continue:
                // each transform depends on the previous one, and the outputs may need the transformed data
                t.apply(&mut measurements)?;
            }
            tx.send(measurements).map_err(|e| {
                TransformError::with_source(TransformErrorKind::Unrecoverable, "sending the measurements failed", e)
            })?;
        } else {
            log::warn!("The channel connected to the transform step has been closed, the transforms will stop.");
            break;
        }
    }
    Ok(())
}

/// A command for an output.
#[derive(Clone, PartialEq, Eq)]
pub enum OutputCmd {
    Run,
    Pause,
    Stop,
}

async fn run_output_from_broadcast(
    mut output: Box<dyn Output>,
    mut rx: broadcast::Receiver<MeasurementBuffer>,
    mut commands: watch::Receiver<OutputCmd>,
) -> Result<(), WriteError> {
    // Two possible designs:
    // A) Use one mpsc channel + one shared variable that contains the current command,
    // - when a message is received, check the command and act accordingly
    // - to change the command, update the variable and send a special message through the channel
    // In this alternative design, each Output would have one mpsc channel, and the Transform step would call send() or try_send() on each of them.
    //
    // B) use a broadcast + watch, where the broadcast discards old values when a receiver (output) lags behind,
    // instead of either (with option A):
    // - preventing the transform from running (mpsc channel's send() blocks when the queue is full).
    // - losing the most recent messages in transform, for one output. Other outputs that are not lagging behind will receive all messages fine, since try_send() does not block, the problem is: what to do with messages that could not be sent, when try_send() fails?)
    loop {
        tokio::select! {
            received_cmd = commands.changed() => {
                // Process new command, clone it to quickly end the borrow (which releases the internal lock as suggested by the doc)
                match received_cmd.map(|_| commands.borrow().clone()) {
                    Ok(OutputCmd::Run) => (), // continue running
                    Ok(OutputCmd::Pause) => {
                        // wait for the command to change
                        match commands.wait_for(|cmd| cmd != &OutputCmd::Pause).await {
                            Ok(new_cmd) => match *new_cmd {
                                OutputCmd::Run => (), // exit the wait
                                OutputCmd::Stop => break, // stop the loop
                                OutputCmd::Pause => unreachable!(),
                            },
                            Err(_) => todo!("watch channel closed"),
                        }
                    },
                    Ok(OutputCmd::Stop) => break, // stop the loop
                    Err(_) => todo!("watch channel closed")
                }
            },
            received_msg = rx.recv() => {
                match received_msg {
                    Ok(measurements) => {
                        // output.write() is blocking, do it in a dedicated thread
                        // Output is not Sync, move the value to the future and back
                        let res = tokio::task::spawn_blocking(move || {
                            (output.write(&measurements), output)
                        }).await;
                        match res {
                            Ok((write_res, out)) => {
                                output = out;
                                if let Err(e) = write_res {
                                    log::error!("Output failed: {:?}", e); // todo give a name to the output
                                }
                            },
                            Err(await_err) => {
                                if await_err.is_panic() {
                                    return Err(WriteError::with_source(super::WriteErrorKind::Unrecoverable, "The blocking writing task panicked.", await_err))
                                } else {
                                    todo!("unhandled error")
                                }
                            },
                        }
                    },
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!("Output is too slow, it lost the oldest {n} messages.");
                    },
                    Err(broadcast::error::RecvError::Closed) => {
                        log::warn!("The channel connected to output was closed, it will now stop.");
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}
