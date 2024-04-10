#ifndef __ALUMET_API_H
#define __ALUMET_API_H

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#define PLUGIN_API __attribute__((visibility("default")))

/**
 * Enum of the possible measurement types.
 */
typedef enum WrappedMeasurementType {
  WrappedMeasurementType_F64,
  WrappedMeasurementType_U64,
} WrappedMeasurementType;

/**
 * Structure passed to plugins for the start-up phase.
 *
 * It allows the plugins to perform some actions before starting the measurment pipeline,
 * such as registering new measurement sources.
 *
 * Note for applications: an `AlumetStart` should not be directly created, use [`PluginStartup`] instead.
 */
typedef struct AlumetStart AlumetStart;

typedef struct ConfigArray ConfigArray;

typedef struct ConfigTable ConfigTable;

/**
 * An accumulator stores measured data points.
 * Unlike a [`MeasurementBuffer`], the accumulator only allows to [`push`](MeasurementAccumulator::push) new points, not to modify them.
 */
typedef struct MeasurementAccumulator MeasurementAccumulator;

/**
 * A `MeasurementBuffer` stores measured data points.
 * Unlike a [`MeasurementAccumulator`], the buffer allows to modify the measurements.
 */
typedef struct MeasurementBuffer MeasurementBuffer;

/**
 * A value that has been measured at a given point in time.
 *
 * Measurement points may also have attributes.
 * Only certain types of values and attributes are allowed, see [`MeasurementType`] and [`AttributeValue`].
 */
typedef struct MeasurementPoint MeasurementPoint;

/**
 * FFI equivalent to [`Option<&str>`].
 */
typedef struct NullableAStr {
  uintptr_t len;
  const char *ptr;
  const void *_marker;
} NullableAStr;

/**
 * FFI equivalent to [`&str`].
 */
typedef struct AStr {
  uintptr_t len;
  char *ptr;
  const void *_marker;
} AStr;

/**
 * A metric id without a generic type information.
 *
 * In general, it is preferred to use [`TypedMetricId`] instead.
 */
typedef struct RawMetricId {
  uintptr_t _0;
} RawMetricId;

typedef struct Timestamp {
  uint64_t secs;
  uint32_t nanos;
} Timestamp;

typedef struct FfiResourceId {
  uint8_t bytes[56];
} FfiResourceId;

typedef enum FfiMeasurementValue_Tag {
  FfiMeasurementValue_U64,
  FfiMeasurementValue_F64,
} FfiMeasurementValue_Tag;

typedef struct FfiMeasurementValue {
  FfiMeasurementValue_Tag tag;
  union {
    struct {
      uint64_t u64;
    };
    struct {
      double f64;
    };
  };
} FfiMeasurementValue;

/**
 * FFI equivalent to [`String`].
 *
 * When modifying an AString, you must ensure that it remains valid UTF-8.
 */
typedef struct AString {
  uintptr_t len;
  uintptr_t capacity;
  char *ptr;
} AString;

typedef void (*ForeachPointFn)(void*, const struct MeasurementPoint*);

/**
 * Id of a custom unit.
 *
 * Custom units can be registered by plugins using [`AlumetStart::create_unit`](crate::plugin::AlumetStart::create_unit).
 */
typedef struct CustomUnitId {
  uint32_t _0;
} CustomUnitId;

/**
 * A unit of measurement.
 *
 * Some common units of the SI are provided as plain enum variants, such as `Unit::Second`.
 */
enum Unit_Tag {
  /**
   * Indicates a dimensionless value. This is suitable for counters.
   */
  Unit_Unity,
  /**
   * Standard unit of **time**.
   */
  Unit_Second,
  /**
   * Standard unit of **power**.
   */
  Unit_Watt,
  /**
   * Standard unit of **energy**.
   */
  Unit_Joule,
  /**
   * Electric tension (aka voltage)
   */
  Unit_Volt,
  /**
   * Intensity of an electric current
   */
  Unit_Ampere,
  /**
   * Frequency (1 Hz = 1/second)
   */
  Unit_Hertz,
  /**
   * Temperature in °C
   */
  Unit_DegreeCelsius,
  /**
   * Temperature in °F
   */
  Unit_DegreeFahrenheit,
  /**
   * Energy in Watt-hour (1 W⋅h = 3.6 kiloJoule = 3.6 × 10^3 Joules)
   */
  Unit_WattHour,
  /**
   * A custom unit
   */
  Unit_Custom,
};
typedef uint8_t Unit_Tag;

typedef union Unit {
  Unit_Tag tag;
  struct {
    Unit_Tag custom_tag;
    struct CustomUnitId custom;
  };
} Unit;

typedef void (*SourcePollFn)(void *instance,
                             struct MeasurementAccumulator *buffer,
                             struct Timestamp timestamp);

typedef void (*NullableDropFn)(void *instance);

typedef void (*TransformApplyFn)(void *instance, struct MeasurementBuffer *buffer);

typedef void (*OutputWriteFn)(void *instance, const struct MeasurementBuffer *buffer);

struct NullableAStr config_string_in(const struct ConfigTable *table, struct AStr key);

const char *config_cstring_in(const struct ConfigTable *table, struct AStr key);

const int64_t *config_int_in(const struct ConfigTable *table, struct AStr key);

const bool *config_bool_in(const struct ConfigTable *table, struct AStr key);

const double *config_float_in(const struct ConfigTable *table, struct AStr key);

const struct ConfigArray *config_array_in(const struct ConfigTable *table, struct AStr key);

const struct ConfigTable *config_table_in(const struct ConfigTable *table, struct AStr key);

struct NullableAStr config_string_at(struct ConfigArray *array, uintptr_t index);

const char *config_cstring_at(const struct ConfigArray *array, uintptr_t index);

const int64_t *config_int_at(const struct ConfigArray *array, uintptr_t index);

const bool *config_bool_at(const struct ConfigArray *array, uintptr_t index);

const double *config_float_at(const struct ConfigArray *array, uintptr_t index);

const struct ConfigArray *config_array_at(const struct ConfigArray *array, uintptr_t index);

const struct ConfigTable *config_table_at(const struct ConfigArray *array, uintptr_t index);

struct AStr metric_name(struct RawMetricId metric);

struct Timestamp *system_time_now(void);

struct MeasurementPoint *mpoint_new_u64(struct Timestamp timestamp,
                                        struct RawMetricId metric,
                                        struct FfiResourceId resource,
                                        uint64_t value);

struct MeasurementPoint *mpoint_new_f64(struct Timestamp timestamp,
                                        struct RawMetricId metric,
                                        struct FfiResourceId resource,
                                        double value);

/**
 * Free a MeasurementPoint.
 * Do **not** call this function after pushing a point with [`mbuffer_push`] or [`maccumulator_push`].
 */
void mpoint_free(struct MeasurementPoint *point);

void mpoint_attr_u64(struct MeasurementPoint *point, struct AStr key, uint64_t value);

void mpoint_attr_f64(struct MeasurementPoint *point, struct AStr key, double value);

void mpoint_attr_bool(struct MeasurementPoint *point, struct AStr key, bool value);

void mpoint_attr_str(struct MeasurementPoint *point, struct AStr key, struct AStr value);

struct RawMetricId mpoint_metric(const struct MeasurementPoint *point);

struct FfiMeasurementValue mpoint_value(const struct MeasurementPoint *point);

struct Timestamp mpoint_timestamp(const struct MeasurementPoint *point);

struct FfiResourceId mpoint_resource(const struct MeasurementPoint *point);

struct AString mpoint_resource_kind(const struct MeasurementPoint *point);

struct AString mpoint_resource_id(const struct MeasurementPoint *point);

uintptr_t mbuffer_len(const struct MeasurementBuffer *buf);

void mbuffer_reserve(struct MeasurementBuffer *buf, uintptr_t additional);

/**
 * Iterates on a [`MeasurementBuffer`] by calling `f(data, point)` for each point of the buffer.
 */
void mbuffer_foreach(const struct MeasurementBuffer *buf, void *data, ForeachPointFn f);

/**
 * Adds a measurement to the buffer.
 * The point is consumed in the operation, you must **not** use it afterwards.
 */
void mbuffer_push(struct MeasurementBuffer *buf, struct MeasurementPoint *point);

/**
 * Adds a measurement to the accumulator.
 * The point is consumed in the operation, you must **not** use it afterwards.
 */
void maccumulator_push(struct MeasurementAccumulator *buf, struct MeasurementPoint *point);

struct RawMetricId alumet_create_metric(struct AlumetStart *alumet,
                                        struct AStr name,
                                        enum WrappedMeasurementType value_type,
                                        union Unit unit,
                                        struct AStr description);

struct RawMetricId alumet_create_metric_c(struct AlumetStart *alumet,
                                          const char *name,
                                          enum WrappedMeasurementType value_type,
                                          union Unit unit,
                                          const char *description);

void alumet_add_source(struct AlumetStart *alumet,
                       void *source_data,
                       SourcePollFn source_poll_fn,
                       NullableDropFn source_drop_fn);

void alumet_add_transform(struct AlumetStart *alumet,
                          void *transform_data,
                          TransformApplyFn transform_apply_fn,
                          NullableDropFn transform_drop_fn);

void alumet_add_output(struct AlumetStart *alumet,
                       void *output_data,
                       OutputWriteFn output_write_fn,
                       NullableDropFn output_drop_fn);

struct FfiResourceId resource_new_local_machine(void);

struct FfiResourceId resource_new_cpu_package(uint32_t pkg_id);

/**
 * Creates a new `AString` from a C string `chars`, which must be null-terminated.
 *
 * The returned `AString` is a copy of the C string.
 * To free the `AString`, use [`astring_free`].
 */
struct AString astring(const char *chars);

struct AString astr_copy(struct AStr astr);

struct AString astr_copy_nonnull(struct NullableAStr astr);

struct AStr astr(const char *chars);

struct AStr astring_ref(struct AString string);

/**
 * Frees a `AString`.
 */
void astring_free(struct AString string);

#endif
