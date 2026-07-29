// Broad semantic coverage for the GPU bytecode expression VM.

#include <sys/wait.h>
#include <unistd.h>

#include <array>
#include <cerrno>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <limits>
#include <utility>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"

namespace {

int failures = 0;
size_t check_count = 0;

#define CHECK(label, condition)                  \
  do {                                           \
    ++check_count;                               \
    if (!(condition)) {                          \
      std::fprintf(stderr, "FAIL: %s\n", label); \
      ++failures;                                \
    }                                            \
  } while (0)

pgaccel_val null_value() {
  pgaccel_val value{};
  value.tag = PGACCEL_VAL_NULL;
  return value;
}

pgaccel_val bool_value(bool input) {
  pgaccel_val value{};
  value.tag = PGACCEL_VAL_BOOL;
  value.data.b = input;
  return value;
}

pgaccel_val i32_value(int32_t input) {
  pgaccel_val value{};
  value.tag = PGACCEL_VAL_INT32;
  value.data.i32 = input;
  return value;
}

pgaccel_val i64_value(int64_t input) {
  pgaccel_val value{};
  value.tag = PGACCEL_VAL_INT64;
  value.data.i64 = input;
  return value;
}

pgaccel_val f32_value(float input) {
  pgaccel_val value{};
  value.tag = PGACCEL_VAL_FLOAT32;
  value.data.f32 = input;
  return value;
}

pgaccel_val f64_value(double input) {
  pgaccel_val value{};
  value.tag = PGACCEL_VAL_FLOAT64;
  value.data.f64 = input;
  return value;
}

pgaccel_val date_value(int32_t input) {
  pgaccel_val value{};
  value.tag = PGACCEL_VAL_DATE;
  value.data.i32 = input;
  return value;
}

pgaccel_val timestamp_value(int64_t input) {
  pgaccel_val value{};
  value.tag = PGACCEL_VAL_TIMESTAMP;
  value.data.i64 = input;
  return value;
}

pgaccel_expr_instruction instruction(uint16_t opcode, uint32_t arg = 0) {
  pgaccel_expr_instruction value{};
  value.opcode = opcode;
  value.arg = arg;
  return value;
}

struct Program {
  std::vector<pgaccel_expr_instruction> instructions;
  std::vector<pgaccel_val> constants;
  pgaccel_expr_program abi{};

  Program(std::vector<pgaccel_expr_instruction> ops, std::vector<pgaccel_val> values,
          size_t num_cols)
      : instructions(std::move(ops)), constants(std::move(values)) {
    abi.instructions = instructions.data();
    abi.inst_count = instructions.size();
    abi.const_pool = constants.data();
    abi.const_count = constants.size();
    abi.max_stack = 64;
    abi.num_cols = num_cols;
  }
};

template <typename T>
struct OneColumnBatch {
  std::vector<T> values;
  std::vector<uint8_t> nulls;
  void* data[1]{};
  uint8_t* null_masks[1]{};
  pgaccel_val_tag types[1]{};
  pgaccel_batch abi{};

  OneColumnBatch(std::vector<T> input, pgaccel_val_tag type, std::vector<uint8_t> null_mask = {})
      : values(std::move(input)), nulls(std::move(null_mask)) {
    data[0] = values.data();
    null_masks[0] = nulls.empty() ? nullptr : nulls.data();
    types[0] = type;
    abi.num_rows = values.size();
    abi.num_cols = 1;
    abi.col_data = data;
    abi.col_nulls = null_masks;
    abi.col_types = types;
  }
};

struct Projection {
  pgaccel_status status = PGACCEL_ERROR;
  std::vector<pgaccel_val> values;
  std::vector<uint8_t> uncertain;
};

Projection project(Program& program, pgaccel_batch& batch) {
  Projection result;
  result.values.resize(batch.num_rows);
  result.uncertain.assign(batch.num_rows, 0xff);
  result.status = pgaccel_expr_eval_project(&program.abi, &batch, result.values.data(),
                                            result.uncertain.data());
  return result;
}

std::vector<int8_t> predicate(Program& program, pgaccel_batch& batch) {
  std::vector<int8_t> result(batch.num_rows, 99);
  CHECK("predicate status",
        pgaccel_expr_eval_predicate(&program.abi, &batch, result.data()) == PGACCEL_OK);
  return result;
}

struct ProgramBuilder {
  struct CheckSpec {
    uint16_t opcode;
    std::array<pgaccel_val, 2> inputs;
    size_t input_count;
    pgaccel_val expected;
    bool expect_null;
  };

  std::vector<pgaccel_expr_instruction> instructions;
  std::vector<pgaccel_val> constants;
  std::vector<CheckSpec> check_specs;

  void op(uint16_t opcode, uint32_t arg = 0) { instructions.push_back(instruction(opcode, arg)); }

  void push(pgaccel_val value) {
    const uint32_t index = static_cast<uint32_t>(constants.size());
    constants.push_back(value);
    op(PGACCEL_EXPR_OP_LOAD_CONST, index);
  }

  void begin_checks() { push(bool_value(true)); }

  void expect_binary(uint16_t opcode, pgaccel_val left, pgaccel_val right, pgaccel_val expected) {
    check_specs.push_back(CheckSpec{opcode, {left, right}, 2, expected, false});
    push(left);
    push(right);
    op(opcode);
    push(expected);
    op(PGACCEL_EXPR_OP_EQ);
    op(PGACCEL_EXPR_OP_AND);
  }

  void expect_binary_null(uint16_t opcode, pgaccel_val left, pgaccel_val right) {
    check_specs.push_back(CheckSpec{opcode, {left, right}, 2, null_value(), true});
    push(left);
    push(right);
    op(opcode);
    op(PGACCEL_EXPR_OP_IS_NULL);
    op(PGACCEL_EXPR_OP_AND);
  }

  void expect_unary(uint16_t opcode, pgaccel_val input, pgaccel_val expected) {
    check_specs.push_back(CheckSpec{opcode, {input, null_value()}, 1, expected, false});
    push(input);
    op(opcode);
    push(expected);
    op(PGACCEL_EXPR_OP_EQ);
    op(PGACCEL_EXPR_OP_AND);
  }

  void expect_unary_null(uint16_t opcode, pgaccel_val input) {
    check_specs.push_back(CheckSpec{opcode, {input, null_value()}, 1, null_value(), true});
    push(input);
    op(opcode);
    op(PGACCEL_EXPR_OP_IS_NULL);
    op(PGACCEL_EXPR_OP_AND);
  }
};

double value_as_f64(const pgaccel_val& value) {
  switch (value.tag) {
    case PGACCEL_VAL_BOOL:
      return value.data.b ? 1.0 : 0.0;
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_DATE:
      return static_cast<double>(value.data.i32);
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_TIMESTAMP:
      return static_cast<double>(value.data.i64);
    case PGACCEL_VAL_FLOAT32:
      return static_cast<double>(value.data.f32);
    case PGACCEL_VAL_FLOAT64:
      return value.data.f64;
    default:
      return 0.0;
  }
}

bool values_equal_for_vm(const pgaccel_val& actual, const pgaccel_val& expected) {
  if (actual.tag == PGACCEL_VAL_NULL || expected.tag == PGACCEL_VAL_NULL)
    return false;
  if ((actual.tag == PGACCEL_VAL_INT32 || actual.tag == PGACCEL_VAL_INT64) &&
      (expected.tag == PGACCEL_VAL_INT32 || expected.tag == PGACCEL_VAL_INT64)) {
    const int64_t left =
        actual.tag == PGACCEL_VAL_INT32 ? static_cast<int64_t>(actual.data.i32) : actual.data.i64;
    const int64_t right = expected.tag == PGACCEL_VAL_INT32
                              ? static_cast<int64_t>(expected.data.i32)
                              : expected.data.i64;
    return left == right;
  }
  if (actual.tag == PGACCEL_VAL_DATE && expected.tag == PGACCEL_VAL_DATE)
    return actual.data.i32 == expected.data.i32;
  if (actual.tag == PGACCEL_VAL_TIMESTAMP && expected.tag == PGACCEL_VAL_TIMESTAMP)
    return actual.data.i64 == expected.data.i64;
  const double left = value_as_f64(actual);
  const double right = value_as_f64(expected);
  return (std::isnan(left) && std::isnan(right)) || left == right;
}

void print_value(const pgaccel_val& value) {
  switch (value.tag) {
    case PGACCEL_VAL_NULL:
      std::fprintf(stderr, "NULL");
      break;
    case PGACCEL_VAL_BOOL:
      std::fprintf(stderr, "BOOL(%d)", value.data.b);
      break;
    case PGACCEL_VAL_INT32:
      std::fprintf(stderr, "INT32(%d)", value.data.i32);
      break;
    case PGACCEL_VAL_INT64:
      std::fprintf(stderr, "INT64(%lld)", static_cast<long long>(value.data.i64));
      break;
    case PGACCEL_VAL_FLOAT32:
      std::fprintf(stderr, "FLOAT32(%.9g)", value.data.f32);
      break;
    case PGACCEL_VAL_FLOAT64:
      std::fprintf(stderr, "FLOAT64(%.17g)", value.data.f64);
      break;
    default:
      std::fprintf(stderr, "TAG(%u)", static_cast<unsigned>(value.tag));
      break;
  }
}

void diagnose_check_program(const char* label, const std::vector<ProgramBuilder::CheckSpec>& specs,
                            pgaccel_batch& batch, bool expected_uncertain) {
  for (size_t index = 0; index < specs.size(); ++index) {
    const ProgramBuilder::CheckSpec& spec = specs[index];
    std::vector<pgaccel_expr_instruction> instructions;
    std::vector<pgaccel_val> constants;
    for (size_t input = 0; input < spec.input_count; ++input) {
      constants.push_back(spec.inputs[input]);
      instructions.push_back(
          instruction(PGACCEL_EXPR_OP_LOAD_CONST, static_cast<uint32_t>(constants.size() - 1)));
    }
    instructions.push_back(instruction(spec.opcode));
    Program program(std::move(instructions), std::move(constants), batch.num_cols);
    const Projection actual = project(program, batch);
    const bool valid_result = actual.status == PGACCEL_OK &&
                              actual.values.size() == batch.num_rows &&
                              actual.uncertain.size() == batch.num_rows && !actual.values.empty();
    const bool value_matches =
        valid_result && (spec.expect_null ? actual.values[0].tag == PGACCEL_VAL_NULL
                                          : values_equal_for_vm(actual.values[0], spec.expected));
    const bool uncertain_matches =
        valid_result && actual.uncertain[0] == static_cast<uint8_t>(expected_uncertain);
    if (value_matches && uncertain_matches)
      continue;

    std::fprintf(stderr, "DIAG: %s check=%zu opcode=%u status=%d actual=", label, index,
                 static_cast<unsigned>(spec.opcode), static_cast<int>(actual.status));
    if (valid_result)
      print_value(actual.values[0]);
    else
      std::fprintf(stderr, "<unavailable>");
    std::fprintf(stderr, " uncertain=%u expected=", valid_result ? actual.uncertain[0] : 0xff);
    print_value(spec.expected);
    std::fprintf(stderr, " expected_uncertain=%u\n", static_cast<unsigned>(expected_uncertain));
  }
}

void expect_check_program(const char* label, ProgramBuilder& builder, pgaccel_batch& batch,
                          bool expected_uncertain = false) {
  const std::vector<ProgramBuilder::CheckSpec> check_specs = builder.check_specs;
  Program program(std::move(builder.instructions), std::move(builder.constants), batch.num_cols);
  Projection result = project(program, batch);
  bool passed = result.status == PGACCEL_OK && result.values.size() == batch.num_rows &&
                result.uncertain.size() == batch.num_rows;
  CHECK(label, passed);
  for (size_t row = 0; row < result.values.size(); ++row) {
    const bool row_passed = result.values[row].tag == PGACCEL_VAL_BOOL &&
                            result.values[row].data.b &&
                            result.uncertain[row] == static_cast<uint8_t>(expected_uncertain);
    CHECK(label, row_passed);
    passed = passed && row_passed;
  }
  if (!passed && !check_specs.empty())
    diagnose_check_program(label, check_specs, batch, expected_uncertain);
}

void force_extended_tier(ProgramBuilder& builder) {
  const size_t jump = builder.instructions.size();
  builder.op(PGACCEL_EXPR_OP_JUMP);
  builder.push(f64_value(4.0));
  builder.op(PGACCEL_EXPR_OP_SQRT_F64);
  builder.instructions[jump].arg = static_cast<uint32_t>(builder.instructions.size());
}

void test_integer_boundaries() {
  struct BoundaryCase {
    uint16_t opcode;
    int64_t left;
    int64_t right;
    int64_t expected;
  };
  constexpr std::array<BoundaryCase, 10> cases = {
      BoundaryCase{PGACCEL_EXPR_OP_ADD_I64, INT64_MAX, 0, INT64_MAX},
      BoundaryCase{PGACCEL_EXPR_OP_ADD_I64, INT64_MIN, 0, INT64_MIN},
      BoundaryCase{PGACCEL_EXPR_OP_SUB_I64, INT64_MAX, 0, INT64_MAX},
      BoundaryCase{PGACCEL_EXPR_OP_SUB_I64, INT64_MIN, 0, INT64_MIN},
      BoundaryCase{PGACCEL_EXPR_OP_MUL_I64, INT64_MAX, 1, INT64_MAX},
      BoundaryCase{PGACCEL_EXPR_OP_MUL_I64, INT64_MAX, -1, -INT64_MAX},
      BoundaryCase{PGACCEL_EXPR_OP_MUL_I64, INT64_MIN, 1, INT64_MIN},
      BoundaryCase{PGACCEL_EXPR_OP_MUL_I64, 0, INT64_MIN, 0},
      BoundaryCase{PGACCEL_EXPR_OP_DIV_I64, INT64_MIN, 1, INT64_MIN},
      BoundaryCase{PGACCEL_EXPR_OP_MOD_I64, INT64_MIN, 2, 0},
  };

  std::array<int32_t, cases.size()> case_ids{};
  std::array<int64_t, cases.size()> left{};
  std::array<int64_t, cases.size()> right{};
  for (size_t i = 0; i < cases.size(); ++i) {
    case_ids[i] = static_cast<int32_t>(i);
    left[i] = cases[i].left;
    right[i] = cases[i].right;
  }
  void* data[] = {case_ids.data(), left.data(), right.data()};
  pgaccel_val_tag types[] = {PGACCEL_VAL_INT32, PGACCEL_VAL_INT64, PGACCEL_VAL_INT64};
  pgaccel_batch batch{cases.size(), 3, data, nullptr, types};

  std::vector<pgaccel_expr_instruction> instructions;
  std::vector<pgaccel_val> constants;
  std::vector<size_t> end_jumps;
  for (size_t i = 0; i < cases.size(); ++i) {
    instructions.push_back(instruction(PGACCEL_EXPR_OP_LOAD_COL, 0));
    constants.push_back(i32_value(static_cast<int32_t>(i)));
    instructions.push_back(
        instruction(PGACCEL_EXPR_OP_LOAD_CONST, static_cast<uint32_t>(constants.size() - 1)));
    instructions.push_back(instruction(PGACCEL_EXPR_OP_EQ));
    const size_t next_case_jump = instructions.size();
    instructions.push_back(instruction(PGACCEL_EXPR_OP_JUMP_IF_FALSE));
    instructions.push_back(instruction(PGACCEL_EXPR_OP_LOAD_COL, 1));
    instructions.push_back(instruction(PGACCEL_EXPR_OP_LOAD_COL, 2));
    instructions.push_back(instruction(cases[i].opcode));
    end_jumps.push_back(instructions.size());
    instructions.push_back(instruction(PGACCEL_EXPR_OP_JUMP));
    instructions[next_case_jump].arg = static_cast<uint32_t>(instructions.size());
  }
  instructions.push_back(instruction(PGACCEL_EXPR_OP_LOAD_NULL));
  const uint32_t end = static_cast<uint32_t>(instructions.size());
  for (const size_t jump : end_jumps)
    instructions[jump].arg = end;

  Program program(std::move(instructions), std::move(constants), 3);
  const Projection result = project(program, batch);
  CHECK("i64 exact boundary status",
        result.status == PGACCEL_OK && result.uncertain == std::vector<uint8_t>(cases.size(), 0));
  for (size_t i = 0; i < cases.size(); ++i) {
    CHECK("i64 exact boundary value", result.values[i].tag == PGACCEL_VAL_INT64 &&
                                          result.values[i].data.i64 == cases[i].expected);
  }
}

void test_compact_arithmetic_matrix() {
  OneColumnBatch<int32_t> batch({1}, PGACCEL_VAL_INT32);
  const float inf32 = std::numeric_limits<float>::infinity();
  const float nan32 = std::numeric_limits<float>::quiet_NaN();
  const double inf64 = std::numeric_limits<double>::infinity();
  const double nan64 = std::numeric_limits<double>::quiet_NaN();
  ProgramBuilder checks;
  checks.begin_checks();

  checks.expect_binary(PGACCEL_EXPR_OP_ADD_I32, i32_value(7), i32_value(5), i32_value(12));
  checks.expect_binary(PGACCEL_EXPR_OP_ADD_I32, i32_value(INT32_MAX), i32_value(0),
                       i32_value(INT32_MAX));
  checks.expect_binary(PGACCEL_EXPR_OP_ADD_I64, i64_value(-9), i64_value(4), i64_value(-5));
  checks.expect_binary(PGACCEL_EXPR_OP_ADD_F32, f32_value(1.25f), f32_value(2.5f),
                       f32_value(3.75f));
  checks.expect_binary(PGACCEL_EXPR_OP_ADD_F64, f64_value(-1.5), f64_value(4.0), f64_value(2.5));
  checks.expect_binary(PGACCEL_EXPR_OP_ADD_F32, f32_value(inf32), f32_value(1.0f),
                       f32_value(inf32));
  checks.expect_binary(PGACCEL_EXPR_OP_ADD_F32, f32_value(nan32), f32_value(1.0f),
                       f32_value(nan32));
  checks.expect_binary(PGACCEL_EXPR_OP_ADD_F64, f64_value(nan64), f64_value(1.0), f64_value(nan64));

  checks.expect_binary(PGACCEL_EXPR_OP_SUB_I32, i32_value(7), i32_value(12), i32_value(-5));
  checks.expect_binary(PGACCEL_EXPR_OP_SUB_I32, i32_value(INT32_MIN), i32_value(0),
                       i32_value(INT32_MIN));
  checks.expect_binary(PGACCEL_EXPR_OP_SUB_I64, i64_value(9), i64_value(14), i64_value(-5));
  checks.expect_binary(PGACCEL_EXPR_OP_SUB_F32, f32_value(8.5f), f32_value(2.25f),
                       f32_value(6.25f));
  checks.expect_binary(PGACCEL_EXPR_OP_SUB_F64, f64_value(8.5), f64_value(2.25), f64_value(6.25));
  checks.expect_binary(PGACCEL_EXPR_OP_SUB_F32, f32_value(-inf32), f32_value(1.0f),
                       f32_value(-inf32));
  checks.expect_binary(PGACCEL_EXPR_OP_SUB_F64, f64_value(inf64), f64_value(-1.0),
                       f64_value(inf64));

  checks.expect_binary(PGACCEL_EXPR_OP_MUL_I32, i32_value(-7), i32_value(6), i32_value(-42));
  checks.expect_binary(PGACCEL_EXPR_OP_MUL_I32, i32_value(INT32_MIN), i32_value(1),
                       i32_value(INT32_MIN));
  checks.expect_binary(PGACCEL_EXPR_OP_MUL_I64, i64_value(-7), i64_value(6), i64_value(-42));
  checks.expect_binary(PGACCEL_EXPR_OP_MUL_I64, i64_value(-7), i64_value(-6), i64_value(42));
  checks.expect_binary(PGACCEL_EXPR_OP_MUL_I64, i64_value(99), i64_value(0), i64_value(0));
  checks.expect_binary(PGACCEL_EXPR_OP_MUL_F32, f32_value(1.5f), f32_value(4.0f), f32_value(6.0f));
  checks.expect_binary(PGACCEL_EXPR_OP_MUL_F64, f64_value(-1.5), f64_value(4.0), f64_value(-6.0));
  checks.expect_binary(PGACCEL_EXPR_OP_MUL_F32, f32_value(inf32), f32_value(2.0f),
                       f32_value(inf32));
  checks.expect_binary(PGACCEL_EXPR_OP_MUL_F64, f64_value(-inf64), f64_value(2.0),
                       f64_value(-inf64));

  checks.expect_binary(PGACCEL_EXPR_OP_DIV_I32, i32_value(-9), i32_value(2), i32_value(-4));
  checks.expect_binary(PGACCEL_EXPR_OP_DIV_I32, i32_value(INT32_MIN), i32_value(1),
                       i32_value(INT32_MIN));
  checks.expect_binary(PGACCEL_EXPR_OP_DIV_I64, i64_value(20), i64_value(-3), i64_value(-6));
  checks.expect_binary(PGACCEL_EXPR_OP_DIV_F32, f32_value(7.5f), f32_value(2.5f), f32_value(3.0f));
  checks.expect_binary(PGACCEL_EXPR_OP_DIV_F64, f64_value(7.5), f64_value(2.5), f64_value(3.0));
  checks.expect_binary(PGACCEL_EXPR_OP_DIV_F32, f32_value(inf32), f32_value(2.0f),
                       f32_value(inf32));
  checks.expect_binary(PGACCEL_EXPR_OP_DIV_F64, f64_value(nan64), f64_value(2.0), f64_value(nan64));
  checks.expect_binary(PGACCEL_EXPR_OP_MOD_I32, i32_value(-17), i32_value(5), i32_value(-2));
  checks.expect_binary(PGACCEL_EXPR_OP_MOD_I32, i32_value(INT32_MIN), i32_value(2), i32_value(0));
  checks.expect_binary(PGACCEL_EXPR_OP_MOD_I64, i64_value(17), i64_value(-5), i64_value(2));

  checks.expect_unary(PGACCEL_EXPR_OP_NEG_I32, i32_value(7), i32_value(-7));
  checks.expect_unary(PGACCEL_EXPR_OP_NEG_I64, i64_value(-7), i64_value(7));
  checks.expect_unary(PGACCEL_EXPR_OP_NEG_F32, f32_value(1.25f), f32_value(-1.25f));
  checks.expect_unary(PGACCEL_EXPR_OP_NEG_F64, f64_value(-1.25), f64_value(1.25));

  ProgramBuilder extended_checks = checks;
  force_extended_tier(extended_checks);
  expect_check_program("all arithmetic opcodes in extended tier", extended_checks, batch.abi);
  expect_check_program("all arithmetic opcodes", checks, batch.abi);
}

void test_arithmetic_null_and_error_matrix() {
  OneColumnBatch<int32_t> batch({1}, PGACCEL_VAL_INT32);
  ProgramBuilder nulls;
  nulls.begin_checks();
  nulls.expect_binary_null(PGACCEL_EXPR_OP_ADD_I32, null_value(), i32_value(1));
  nulls.expect_binary_null(PGACCEL_EXPR_OP_ADD_I64, i64_value(1), null_value());
  nulls.expect_binary_null(PGACCEL_EXPR_OP_ADD_F32, null_value(), f32_value(1.0f));
  nulls.expect_binary_null(PGACCEL_EXPR_OP_ADD_F64, f64_value(1.0), null_value());
  nulls.expect_binary_null(PGACCEL_EXPR_OP_SUB_I32, null_value(), i32_value(1));
  nulls.expect_binary_null(PGACCEL_EXPR_OP_SUB_I64, i64_value(1), null_value());
  nulls.expect_binary_null(PGACCEL_EXPR_OP_SUB_F32, null_value(), f32_value(1.0f));
  nulls.expect_binary_null(PGACCEL_EXPR_OP_SUB_F64, f64_value(1.0), null_value());
  nulls.expect_binary_null(PGACCEL_EXPR_OP_MUL_I32, null_value(), i32_value(1));
  nulls.expect_binary_null(PGACCEL_EXPR_OP_MUL_I64, i64_value(1), null_value());
  nulls.expect_binary_null(PGACCEL_EXPR_OP_MUL_F32, null_value(), f32_value(1.0f));
  nulls.expect_binary_null(PGACCEL_EXPR_OP_MUL_F64, f64_value(1.0), null_value());
  nulls.expect_binary_null(PGACCEL_EXPR_OP_DIV_I32, null_value(), i32_value(1));
  nulls.expect_binary_null(PGACCEL_EXPR_OP_DIV_I64, i64_value(1), null_value());
  nulls.expect_binary_null(PGACCEL_EXPR_OP_DIV_F32, null_value(), f32_value(1.0f));
  nulls.expect_binary_null(PGACCEL_EXPR_OP_DIV_F64, f64_value(1.0), null_value());
  nulls.expect_binary_null(PGACCEL_EXPR_OP_MOD_I32, null_value(), i32_value(1));
  nulls.expect_binary_null(PGACCEL_EXPR_OP_MOD_I64, i64_value(1), null_value());
  nulls.expect_unary_null(PGACCEL_EXPR_OP_NEG_I32, null_value());
  nulls.expect_unary_null(PGACCEL_EXPR_OP_NEG_I64, null_value());
  nulls.expect_unary_null(PGACCEL_EXPR_OP_NEG_F32, null_value());
  nulls.expect_unary_null(PGACCEL_EXPR_OP_NEG_F64, null_value());
  ProgramBuilder extended_nulls = nulls;
  force_extended_tier(extended_nulls);
  expect_check_program("arithmetic NULL propagation in extended tier", extended_nulls, batch.abi);
  expect_check_program("arithmetic NULL propagation", nulls, batch.abi);

  ProgramBuilder errors;
  errors.begin_checks();
  const float max32 = std::numeric_limits<float>::max();
  const float min32 = std::numeric_limits<float>::min();
  const double max64 = std::numeric_limits<double>::max();
  const double min64 = std::numeric_limits<double>::min();
  errors.expect_binary_null(PGACCEL_EXPR_OP_ADD_I32, i32_value(INT32_MAX), i32_value(1));
  errors.expect_binary_null(PGACCEL_EXPR_OP_ADD_I32, i32_value(INT32_MIN), i32_value(-1));
  errors.expect_binary_null(PGACCEL_EXPR_OP_ADD_I64, i64_value(INT64_MAX), i64_value(1));
  errors.expect_binary_null(PGACCEL_EXPR_OP_ADD_I64, i64_value(INT64_MIN), i64_value(-1));
  errors.expect_binary_null(PGACCEL_EXPR_OP_SUB_I32, i32_value(INT32_MIN), i32_value(1));
  errors.expect_binary_null(PGACCEL_EXPR_OP_SUB_I32, i32_value(INT32_MAX), i32_value(-1));
  errors.expect_binary_null(PGACCEL_EXPR_OP_SUB_I64, i64_value(INT64_MIN), i64_value(1));
  errors.expect_binary_null(PGACCEL_EXPR_OP_SUB_I64, i64_value(INT64_MAX), i64_value(-1));
  errors.expect_binary_null(PGACCEL_EXPR_OP_MUL_I32, i32_value(INT32_MAX), i32_value(2));
  errors.expect_binary_null(PGACCEL_EXPR_OP_MUL_I32, i32_value(INT32_MIN), i32_value(-1));
  errors.expect_binary_null(PGACCEL_EXPR_OP_MUL_I32, i32_value(INT32_MIN), i32_value(2));
  errors.expect_binary_null(PGACCEL_EXPR_OP_MUL_I64, i64_value(INT64_MAX), i64_value(2));
  errors.expect_binary_null(PGACCEL_EXPR_OP_MUL_I64, i64_value(INT64_MAX), i64_value(-2));
  errors.expect_binary_null(PGACCEL_EXPR_OP_MUL_I64, i64_value(INT64_MIN), i64_value(2));
  errors.expect_binary_null(PGACCEL_EXPR_OP_MUL_I64, i64_value(INT64_MIN), i64_value(-1));
  errors.expect_binary_null(PGACCEL_EXPR_OP_ADD_F32, f32_value(max32), f32_value(max32));
  errors.expect_binary_null(PGACCEL_EXPR_OP_ADD_F64, f64_value(max64), f64_value(max64));
  errors.expect_binary_null(PGACCEL_EXPR_OP_SUB_F32, f32_value(max32), f32_value(-max32));
  errors.expect_binary_null(PGACCEL_EXPR_OP_SUB_F64, f64_value(max64), f64_value(-max64));
  errors.expect_binary_null(PGACCEL_EXPR_OP_MUL_F32, f32_value(max32), f32_value(2.0f));
  errors.expect_binary_null(PGACCEL_EXPR_OP_MUL_F64, f64_value(max64), f64_value(2.0));
  errors.expect_binary_null(PGACCEL_EXPR_OP_DIV_I32, i32_value(1), i32_value(0));
  errors.expect_binary_null(PGACCEL_EXPR_OP_DIV_I32, i32_value(INT32_MIN), i32_value(-1));
  errors.expect_binary_null(PGACCEL_EXPR_OP_DIV_I64, i64_value(1), i64_value(0));
  errors.expect_binary_null(PGACCEL_EXPR_OP_DIV_I64, i64_value(INT64_MIN), i64_value(-1));
  errors.expect_binary_null(PGACCEL_EXPR_OP_DIV_F32, f32_value(1.0f), f32_value(0.0f));
  errors.expect_binary_null(PGACCEL_EXPR_OP_DIV_F32, f32_value(1.0f), f32_value(-0.0f));
  errors.expect_binary_null(PGACCEL_EXPR_OP_DIV_F64, f64_value(1.0), f64_value(0.0));
  errors.expect_binary_null(PGACCEL_EXPR_OP_DIV_F64, f64_value(1.0), f64_value(-0.0));
  errors.expect_binary_null(PGACCEL_EXPR_OP_DIV_F32, f32_value(max32), f32_value(min32));
  errors.expect_binary_null(PGACCEL_EXPR_OP_DIV_F64, f64_value(max64), f64_value(min64));
  errors.expect_binary_null(PGACCEL_EXPR_OP_MOD_I32, i32_value(1), i32_value(0));
  errors.expect_binary_null(PGACCEL_EXPR_OP_MOD_I32, i32_value(INT32_MIN), i32_value(-1));
  errors.expect_binary_null(PGACCEL_EXPR_OP_MOD_I64, i64_value(1), i64_value(0));
  errors.expect_binary_null(PGACCEL_EXPR_OP_MOD_I64, i64_value(INT64_MIN), i64_value(-1));
  errors.expect_unary_null(PGACCEL_EXPR_OP_NEG_I32, i32_value(INT32_MIN));
  errors.expect_unary_null(PGACCEL_EXPR_OP_NEG_I64, i64_value(INT64_MIN));
  errors.expect_unary_null(PGACCEL_EXPR_OP_ABS_I32, i32_value(INT32_MIN));
  errors.expect_unary_null(PGACCEL_EXPR_OP_ABS_I64, i64_value(INT64_MIN));
  ProgramBuilder common_errors = errors;
  expect_check_program("common arithmetic error paths", common_errors, batch.abi, true);

  errors.expect_unary_null(PGACCEL_EXPR_OP_SQRT_F64, f64_value(-1.0));
  errors.expect_binary_null(PGACCEL_EXPR_OP_POW_F64, f64_value(-1.0), f64_value(0.5));
  errors.expect_binary_null(PGACCEL_EXPR_OP_POW_F64, f64_value(1.0e308), f64_value(2.0));
  errors.expect_binary_null(PGACCEL_EXPR_OP_POW_F64, f64_value(0.0), f64_value(-1.0));
  expect_check_program("extended arithmetic and math error paths", errors, batch.abi, true);
}

void test_comparison_matrix() {
  OneColumnBatch<int32_t> batch({1}, PGACCEL_VAL_INT32);
  const double nan = std::numeric_limits<double>::quiet_NaN();
  constexpr int64_t beyond_f64_exact = INT64_C(9007199254740993);
  ProgramBuilder checks;
  checks.begin_checks();
  checks.expect_binary(PGACCEL_EXPR_OP_EQ, i32_value(7), i64_value(7), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_EQ, i64_value(beyond_f64_exact),
                       i64_value(beyond_f64_exact + 1), bool_value(false));
  checks.expect_binary(PGACCEL_EXPR_OP_LT, i64_value(beyond_f64_exact),
                       i64_value(beyond_f64_exact + 1), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_GE, i64_value(beyond_f64_exact + 1),
                       i64_value(beyond_f64_exact), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_NE, timestamp_value(beyond_f64_exact),
                       timestamp_value(beyond_f64_exact + 1), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_LT, timestamp_value(beyond_f64_exact),
                       timestamp_value(beyond_f64_exact + 1), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_EQ, date_value(8766), date_value(8766), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_NE, f32_value(2.0f), f64_value(3.0), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_LT, bool_value(false), i32_value(1), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_LE, i64_value(4), f64_value(4.0), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_GT, f64_value(9.0), f32_value(4.0f), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_GE, i32_value(4), bool_value(true), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_EQ, f64_value(nan), f64_value(nan), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_EQ, f64_value(nan), f64_value(1.0), bool_value(false));
  checks.expect_binary(PGACCEL_EXPR_OP_NE, f64_value(nan), f64_value(1.0), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_LT, f64_value(nan), f64_value(1.0), bool_value(false));
  checks.expect_binary(PGACCEL_EXPR_OP_LT, f64_value(1.0), f64_value(nan), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_LE, f64_value(nan), f64_value(nan), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_GE, f64_value(nan), f64_value(1.0), bool_value(true));
  checks.expect_binary_null(PGACCEL_EXPR_OP_EQ, null_value(), i32_value(1));
  checks.expect_binary_null(PGACCEL_EXPR_OP_NE, i32_value(1), null_value());
  checks.expect_binary_null(PGACCEL_EXPR_OP_LT, null_value(), i32_value(1));
  checks.expect_binary_null(PGACCEL_EXPR_OP_LE, i32_value(1), null_value());
  checks.expect_binary_null(PGACCEL_EXPR_OP_GT, null_value(), i32_value(1));
  checks.expect_binary_null(PGACCEL_EXPR_OP_GE, i32_value(1), null_value());
  ProgramBuilder extended_checks = checks;
  force_extended_tier(extended_checks);
  expect_check_program("comparison and PostgreSQL NaN semantics in extended tier", extended_checks,
                       batch.abi);
  expect_check_program("comparison and PostgreSQL NaN semantics", checks, batch.abi);
}

void test_boolean_cast_and_math_matrix() {
  OneColumnBatch<int32_t> batch({1}, PGACCEL_VAL_INT32);
  const double inf = std::numeric_limits<double>::infinity();
  const double nan = std::numeric_limits<double>::quiet_NaN();
  ProgramBuilder checks;
  checks.begin_checks();
  checks.expect_binary(PGACCEL_EXPR_OP_AND, bool_value(false), null_value(), bool_value(false));
  checks.expect_binary(PGACCEL_EXPR_OP_AND, null_value(), bool_value(false), bool_value(false));
  checks.expect_binary_null(PGACCEL_EXPR_OP_AND, null_value(), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_AND, bool_value(true), bool_value(true), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_AND, i32_value(1), i64_value(0), bool_value(false));
  checks.expect_binary(PGACCEL_EXPR_OP_OR, bool_value(true), null_value(), bool_value(true));
  checks.expect_binary(PGACCEL_EXPR_OP_OR, null_value(), bool_value(true), bool_value(true));
  checks.expect_binary_null(PGACCEL_EXPR_OP_OR, null_value(), bool_value(false));
  checks.expect_binary(PGACCEL_EXPR_OP_OR, bool_value(false), bool_value(false), bool_value(false));
  checks.expect_unary(PGACCEL_EXPR_OP_NOT, bool_value(true), bool_value(false));
  checks.expect_unary_null(PGACCEL_EXPR_OP_NOT, null_value());
  checks.expect_unary(PGACCEL_EXPR_OP_IS_NULL, i32_value(1), bool_value(false));
  checks.expect_unary(PGACCEL_EXPR_OP_IS_NOT_NULL, i32_value(1), bool_value(true));
  checks.expect_unary(PGACCEL_EXPR_OP_IS_NOT_NULL, null_value(), bool_value(false));
  checks.expect_binary(PGACCEL_EXPR_OP_COALESCE, null_value(), i64_value(42), i64_value(42));
  checks.expect_binary(PGACCEL_EXPR_OP_COALESCE, i32_value(7), i32_value(9), i32_value(7));

  checks.expect_unary(PGACCEL_EXPR_OP_CAST_I32_I64, i32_value(-7), i64_value(-7));
  checks.expect_unary(PGACCEL_EXPR_OP_CAST_I32_F64, i32_value(7), f64_value(7.0));
  checks.expect_unary(PGACCEL_EXPR_OP_CAST_I64_F64, i64_value(-9), f64_value(-9.0));
  checks.expect_unary(PGACCEL_EXPR_OP_CAST_F32_F64, f32_value(1.25f), f64_value(1.25));
  checks.expect_unary(PGACCEL_EXPR_OP_CAST_F64_F32, f64_value(1.25), f32_value(1.25f));
  checks.expect_unary(PGACCEL_EXPR_OP_CAST_BOOL_I32, bool_value(true), i32_value(1));
  checks.expect_unary_null(PGACCEL_EXPR_OP_CAST_I32_I64, null_value());
  checks.expect_unary_null(PGACCEL_EXPR_OP_CAST_I32_F64, null_value());
  checks.expect_unary_null(PGACCEL_EXPR_OP_CAST_I64_F64, null_value());
  checks.expect_unary_null(PGACCEL_EXPR_OP_CAST_F32_F64, null_value());
  checks.expect_unary_null(PGACCEL_EXPR_OP_CAST_F64_F32, null_value());
  checks.expect_unary_null(PGACCEL_EXPR_OP_CAST_BOOL_I32, null_value());

  checks.expect_unary(PGACCEL_EXPR_OP_ABS_I32, i32_value(-7), i32_value(7));
  checks.expect_unary(PGACCEL_EXPR_OP_ABS_I64, i64_value(-9), i64_value(9));
  checks.expect_unary(PGACCEL_EXPR_OP_ABS_F64, f64_value(-2.5), f64_value(2.5));
  checks.expect_unary_null(PGACCEL_EXPR_OP_ABS_I32, null_value());
  checks.expect_unary_null(PGACCEL_EXPR_OP_ABS_I64, null_value());
  checks.expect_unary_null(PGACCEL_EXPR_OP_ABS_F64, null_value());
  ProgramBuilder common_checks = checks;
  expect_check_program("boolean, cast, and common math opcodes", common_checks, batch.abi);

  checks.expect_unary(PGACCEL_EXPR_OP_SQRT_F64, f64_value(81.0), f64_value(9.0));
  checks.expect_unary(PGACCEL_EXPR_OP_CEIL_F64, f64_value(2.25), f64_value(3.0));
  checks.expect_unary(PGACCEL_EXPR_OP_FLOOR_F64, f64_value(-2.25), f64_value(-3.0));
  checks.expect_unary(PGACCEL_EXPR_OP_ROUND_F64, f64_value(-2.5), f64_value(-3.0));
  checks.expect_binary(PGACCEL_EXPR_OP_POW_F64, f64_value(2.0), f64_value(3.0), f64_value(8.0));
  checks.expect_binary(PGACCEL_EXPR_OP_POW_F64, f64_value(inf), f64_value(2.0), f64_value(inf));
  checks.expect_binary(PGACCEL_EXPR_OP_POW_F64, f64_value(-inf), f64_value(2.0), f64_value(inf));
  checks.expect_binary(PGACCEL_EXPR_OP_POW_F64, f64_value(-inf), f64_value(3.0), f64_value(-inf));
  checks.expect_binary(PGACCEL_EXPR_OP_POW_F64, f64_value(-inf), f64_value(-2.0), f64_value(0.0));
  checks.expect_binary(PGACCEL_EXPR_OP_POW_F64, f64_value(-inf), f64_value(-3.0), f64_value(-0.0));
  checks.expect_binary(PGACCEL_EXPR_OP_POW_F64, f64_value(-inf), f64_value(2.5), f64_value(inf));
  checks.expect_binary(PGACCEL_EXPR_OP_POW_F64, f64_value(-inf), f64_value(0.0), f64_value(1.0));
  checks.expect_binary(PGACCEL_EXPR_OP_POW_F64, f64_value(-inf), f64_value(inf), f64_value(inf));
  checks.expect_binary(PGACCEL_EXPR_OP_POW_F64, f64_value(-inf), f64_value(-inf), f64_value(0.0));
  checks.expect_binary(PGACCEL_EXPR_OP_POW_F64, f64_value(-inf), f64_value(9007199254740991.0),
                       f64_value(-inf));
  checks.expect_binary(PGACCEL_EXPR_OP_POW_F64, f64_value(-inf), f64_value(9007199254740992.0),
                       f64_value(inf));
  checks.expect_binary(PGACCEL_EXPR_OP_POW_F64, f64_value(-inf), f64_value(nan), f64_value(nan));
  checks.expect_binary(PGACCEL_EXPR_OP_POW_F64, f64_value(nan), f64_value(2.0), f64_value(nan));
  checks.expect_unary_null(PGACCEL_EXPR_OP_SQRT_F64, null_value());
  checks.expect_unary_null(PGACCEL_EXPR_OP_CEIL_F64, null_value());
  checks.expect_unary_null(PGACCEL_EXPR_OP_FLOOR_F64, null_value());
  checks.expect_unary_null(PGACCEL_EXPR_OP_ROUND_F64, null_value());
  checks.expect_binary_null(PGACCEL_EXPR_OP_POW_F64, null_value(), f64_value(2.0));
  expect_check_program("boolean, cast, and math opcodes", checks, batch.abi);
}

void test_comparisons_and_predicates() {
  OneColumnBatch<double> batch({-1.0, 2.0, 9.0, 0.0}, PGACCEL_VAL_FLOAT64, {0, 0, 0, 1});
  Program greater({instruction(PGACCEL_EXPR_OP_LOAD_COL, 0),
                   instruction(PGACCEL_EXPR_OP_LOAD_CONST, 0), instruction(PGACCEL_EXPR_OP_GT)},
                  {f64_value(1.5)}, 1);
  const std::vector<int8_t> selected = predicate(greater, batch.abi);
  CHECK("GT predicate values",
        selected == std::vector<int8_t>({PGACCEL_EXPR_FALSE, PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE,
                                         PGACCEL_EXPR_FALSE}));
}

void test_round() {
  OneColumnBatch<double> batch({-1.5, -0.0, 1.5, 2.4}, PGACCEL_VAL_FLOAT64);
  Program program(
      {instruction(PGACCEL_EXPR_OP_LOAD_COL, 0), instruction(PGACCEL_EXPR_OP_ROUND_F64)}, {}, 1);
  Projection result = project(program, batch.abi);
  CHECK("ROUND status",
        result.status == PGACCEL_OK && result.uncertain == std::vector<uint8_t>({0, 0, 0, 0}));
  CHECK("ROUND values", result.values[0].data.f64 == -2.0 && result.values[1].data.f64 == 0.0 &&
                            std::signbit(result.values[1].data.f64) &&
                            result.values[2].data.f64 == 2.0 && result.values[3].data.f64 == 2.0);
}

void test_extended_math_dispatch_split() {
  struct ExtendedCase {
    uint16_t opcode;
    double input;
    double rhs;
    double expected;
    bool binary;
  };
  constexpr std::array<ExtendedCase, 5> cases = {
      ExtendedCase{PGACCEL_EXPR_OP_SQRT_F64, 81.0, 0.0, 9.0, false},
      ExtendedCase{PGACCEL_EXPR_OP_CEIL_F64, 2.25, 0.0, 3.0, false},
      ExtendedCase{PGACCEL_EXPR_OP_FLOOR_F64, -2.25, 0.0, -3.0, false},
      ExtendedCase{PGACCEL_EXPR_OP_ROUND_F64, -2.5, 0.0, -3.0, false},
      ExtendedCase{PGACCEL_EXPR_OP_POW_F64, 2.0, 3.0, 8.0, true},
  };
  OneColumnBatch<int32_t> batch({1}, PGACCEL_VAL_INT32);

  for (const ExtendedCase& test : cases) {
    std::vector<pgaccel_val> constants = {f64_value(test.input)};
    std::vector<pgaccel_expr_instruction> instructions = {
        instruction(PGACCEL_EXPR_OP_LOAD_CONST, 0)};
    if (test.binary) {
      constants.push_back(f64_value(test.rhs));
      instructions.push_back(instruction(PGACCEL_EXPR_OP_LOAD_CONST, 1));
    }
    instructions.push_back(instruction(test.opcode));

    Program projection_program(instructions, constants, 1);
    const Projection projected = project(projection_program, batch.abi);
    CHECK("isolated extended opcode projection route",
          projected.status == PGACCEL_OK && projected.uncertain == std::vector<uint8_t>({0}) &&
              projected.values[0].tag == PGACCEL_VAL_FLOAT64 &&
              projected.values[0].data.f64 == test.expected);

    constants.push_back(f64_value(test.expected));
    instructions.push_back(
        instruction(PGACCEL_EXPR_OP_LOAD_CONST, static_cast<uint32_t>(constants.size() - 1)));
    instructions.push_back(instruction(PGACCEL_EXPR_OP_EQ));
    Program predicate_program(std::move(instructions), std::move(constants), 1);
    CHECK("isolated extended opcode predicate route",
          predicate(predicate_program, batch.abi) == std::vector<int8_t>({PGACCEL_EXPR_TRUE}));
  }

  Program nested({instruction(PGACCEL_EXPR_OP_LOAD_CONST, 0), instruction(PGACCEL_EXPR_OP_SQRT_F64),
                  instruction(PGACCEL_EXPR_OP_LOAD_CONST, 1), instruction(PGACCEL_EXPR_OP_ADD_F64),
                  instruction(PGACCEL_EXPR_OP_CEIL_F64), instruction(PGACCEL_EXPR_OP_LOAD_CONST, 2),
                  instruction(PGACCEL_EXPR_OP_POW_F64), instruction(PGACCEL_EXPR_OP_SQRT_F64),
                  instruction(PGACCEL_EXPR_OP_FLOOR_F64), instruction(PGACCEL_EXPR_OP_ROUND_F64)},
                 {f64_value(225.0), f64_value(0.25), f64_value(2.0)}, 1);
  const Projection nested_result = project(nested, batch.abi);
  CHECK("nested mixed extended/common projection",
        nested_result.status == PGACCEL_OK &&
            nested_result.uncertain == std::vector<uint8_t>({0}) &&
            nested_result.values[0].tag == PGACCEL_VAL_FLOAT64 &&
            nested_result.values[0].data.f64 == 16.0);

  OneColumnBatch<double> parity_batch({-2.0, -1.0, 0.0, 1.0, 2.0}, PGACCEL_VAL_FLOAT64);
  std::vector<pgaccel_expr_instruction> common = {
      instruction(PGACCEL_EXPR_OP_LOAD_COL, 0), instruction(PGACCEL_EXPR_OP_LOAD_CONST, 0),
      instruction(PGACCEL_EXPR_OP_ADD_F64),     instruction(PGACCEL_EXPR_OP_LOAD_CONST, 1),
      instruction(PGACCEL_EXPR_OP_MUL_F64),     instruction(PGACCEL_EXPR_OP_LOAD_CONST, 2),
      instruction(PGACCEL_EXPR_OP_SUB_F64)};
  const std::vector<pgaccel_val> common_constants = {f64_value(3.0), f64_value(2.0),
                                                     f64_value(6.0)};
  Program lean(common, common_constants, 1);
  const Projection lean_result = project(lean, parity_batch.abi);
  const std::vector<int8_t> lean_predicate = predicate(lean, parity_batch.abi);

  common.push_back(instruction(PGACCEL_EXPR_OP_JUMP, 10));
  common.push_back(instruction(PGACCEL_EXPR_OP_LOAD_CONST, 0));
  common.push_back(instruction(PGACCEL_EXPR_OP_SQRT_F64));
  Program extended(std::move(common), common_constants, 1);
  const Projection extended_result = project(extended, parity_batch.abi);
  CHECK("common projection parity across split",
        lean_result.status == PGACCEL_OK && extended_result.status == PGACCEL_OK &&
            lean_result.uncertain == extended_result.uncertain &&
            lean_result.values.size() == extended_result.values.size());
  for (size_t row = 0; row < lean_result.values.size(); ++row) {
    CHECK("common projection value parity across split",
          lean_result.values[row].tag == PGACCEL_VAL_FLOAT64 &&
              extended_result.values[row].tag == PGACCEL_VAL_FLOAT64 &&
              lean_result.values[row].data.f64 == extended_result.values[row].data.f64);
  }
  CHECK("common predicate parity across split",
        lean_predicate == predicate(extended, parity_batch.abi));

  Program invalid_extended({instruction(PGACCEL_EXPR_OP_LOAD_CONST, 0), instruction(UINT16_MAX),
                            instruction(PGACCEL_EXPR_OP_JUMP, 5),
                            instruction(PGACCEL_EXPR_OP_LOAD_CONST, 1),
                            instruction(PGACCEL_EXPR_OP_SQRT_F64)},
                           {bool_value(true), f64_value(4.0)}, 1);
  const Projection invalid_result = project(invalid_extended, batch.abi);
  CHECK("unknown opcode remains uncertain in extended route",
        invalid_result.status == PGACCEL_OK &&
            invalid_result.uncertain == std::vector<uint8_t>({1}) &&
            invalid_result.values[0].tag == PGACCEL_VAL_BOOL && invalid_result.values[0].data.b);
  CHECK("unknown opcode predicate remains uncertain in extended route",
        predicate(invalid_extended, batch.abi) == std::vector<int8_t>({PGACCEL_EXPR_UNCERTAIN}));
}

void test_basic_expression_tier() {
  constexpr size_t kRows = 4;
  constexpr size_t kColumns = 8;
  std::array<std::array<double, kRows>, kColumns> values{};
  std::array<std::array<uint8_t, kRows>, kColumns> null_bits{};
  std::array<void*, kColumns> data{};
  std::array<uint8_t*, kColumns> nulls{};
  std::array<pgaccel_val_tag, kColumns> types{};
  for (size_t column = 0; column < kColumns; ++column) {
    values[column].fill(static_cast<double>(column + 1));
    data[column] = values[column].data();
    nulls[column] = null_bits[column].data();
    types[column] = PGACCEL_VAL_FLOAT64;
  }
  null_bits[0][1] = 1;
  null_bits[3][2] = 1;
  null_bits[7][3] = 1;
  pgaccel_batch batch{kRows, kColumns, data.data(), nulls.data(), types.data()};

  std::vector<pgaccel_expr_instruction> basic_instructions;
  for (uint32_t column = 0; column < kColumns; ++column) {
    basic_instructions.push_back(instruction(PGACCEL_EXPR_OP_LOAD_COL, column));
    basic_instructions.push_back(instruction(PGACCEL_EXPR_OP_IS_NOT_NULL));
    if (column > 0)
      basic_instructions.push_back(instruction(PGACCEL_EXPR_OP_AND));
  }
  Program basic(basic_instructions, {}, kColumns);
  const Projection basic_projection = project(basic, batch);
  CHECK("basic OOM-shape projection status",
        basic_projection.status == PGACCEL_OK &&
            basic_projection.uncertain == std::vector<uint8_t>(kRows, 0));
  const std::array<bool, kRows> expected = {true, false, false, false};
  for (size_t row = 0; row < kRows; ++row) {
    CHECK("basic OOM-shape projection value",
          basic_projection.values[row].tag == PGACCEL_VAL_BOOL &&
              basic_projection.values[row].data.b == expected[row]);
  }
  const std::vector<int8_t> basic_predicate = predicate(basic, batch);
  CHECK("basic OOM-shape predicate values",
        basic_predicate == std::vector<int8_t>({PGACCEL_EXPR_TRUE, PGACCEL_EXPR_FALSE,
                                                PGACCEL_EXPR_FALSE, PGACCEL_EXPR_FALSE}));

  std::vector<pgaccel_expr_instruction> common_instructions = basic_instructions;
  common_instructions.push_back(
      instruction(PGACCEL_EXPR_OP_JUMP, static_cast<uint32_t>(common_instructions.size() + 2)));
  common_instructions.push_back(instruction(PGACCEL_EXPR_OP_ALWAYS_TRUE));
  Program common(std::move(common_instructions), {}, kColumns);
  const Projection common_projection = project(common, batch);
  const std::vector<int8_t> common_predicate = predicate(common, batch);

  std::vector<pgaccel_expr_instruction> extended_instructions = basic_instructions;
  extended_instructions.push_back(
      instruction(PGACCEL_EXPR_OP_JUMP, static_cast<uint32_t>(extended_instructions.size() + 3)));
  extended_instructions.push_back(instruction(PGACCEL_EXPR_OP_LOAD_COL, 0));
  extended_instructions.push_back(instruction(PGACCEL_EXPR_OP_SQRT_F64));
  Program extended(std::move(extended_instructions), {}, kColumns);
  const Projection extended_projection = project(extended, batch);
  const std::vector<int8_t> extended_predicate = predicate(extended, batch);

  CHECK("basic/common/extended predicate parity",
        basic_predicate == common_predicate && basic_predicate == extended_predicate);
  CHECK("basic/common/extended projection status parity",
        common_projection.status == PGACCEL_OK && extended_projection.status == PGACCEL_OK &&
            basic_projection.uncertain == common_projection.uncertain &&
            basic_projection.uncertain == extended_projection.uncertain);
  for (size_t row = 0; row < kRows; ++row) {
    CHECK("basic/common/extended projection value parity",
          common_projection.values[row].tag == basic_projection.values[row].tag &&
              extended_projection.values[row].tag == basic_projection.values[row].tag &&
              common_projection.values[row].data.b == basic_projection.values[row].data.b &&
              extended_projection.values[row].data.b == basic_projection.values[row].data.b);
  }

  bool left[] = {true, false, true, true, true};
  bool right[] = {true, false, true, false, true};
  uint8_t left_nulls[] = {0, 0, 0, 1, 1};
  uint8_t right_nulls[] = {0, 1, 1, 0, 0};
  void* and_data[] = {left, right};
  uint8_t* and_nulls[] = {left_nulls, right_nulls};
  pgaccel_val_tag and_types[] = {PGACCEL_VAL_BOOL, PGACCEL_VAL_BOOL};
  pgaccel_batch and_batch{5, 2, and_data, and_nulls, and_types};
  Program direct_and({instruction(PGACCEL_EXPR_OP_LOAD_COL, 0),
                      instruction(PGACCEL_EXPR_OP_LOAD_COL, 1), instruction(PGACCEL_EXPR_OP_AND)},
                     {}, 2);
  const Projection and_projection = project(direct_and, and_batch);
  CHECK("basic SQL AND status", and_projection.status == PGACCEL_OK &&
                                    and_projection.uncertain == std::vector<uint8_t>(5, 0));
  CHECK("basic SQL AND values",
        and_projection.values[0].tag == PGACCEL_VAL_BOOL && and_projection.values[0].data.b &&
            and_projection.values[1].tag == PGACCEL_VAL_BOOL && !and_projection.values[1].data.b &&
            and_projection.values[2].tag == PGACCEL_VAL_NULL &&
            and_projection.values[3].tag == PGACCEL_VAL_BOOL && !and_projection.values[3].data.b &&
            and_projection.values[4].tag == PGACCEL_VAL_NULL);
  CHECK("basic SQL AND predicate values",
        predicate(direct_and, and_batch) ==
            std::vector<int8_t>({PGACCEL_EXPR_TRUE, PGACCEL_EXPR_FALSE, PGACCEL_EXPR_FALSE,
                                 PGACCEL_EXPR_FALSE, PGACCEL_EXPR_FALSE}));

  OneColumnBatch<int32_t> one({1}, PGACCEL_VAL_INT32);
  Program malformed({instruction(PGACCEL_EXPR_OP_IS_NOT_NULL)}, {}, 1);
  const Projection malformed_projection = project(malformed, one.abi);
  CHECK("malformed basic program is uncertain",
        malformed_projection.status == PGACCEL_OK &&
            malformed_projection.uncertain == std::vector<uint8_t>({1}) &&
            malformed_projection.values[0].tag == PGACCEL_VAL_NULL);
  CHECK("malformed basic predicate is uncertain",
        predicate(malformed, one.abi) == std::vector<int8_t>({PGACCEL_EXPR_UNCERTAIN}));

  Program malformed_and({instruction(PGACCEL_EXPR_OP_AND)}, {}, 1);
  const Projection malformed_and_projection = project(malformed_and, one.abi);
  CHECK("basic AND underflow is uncertain",
        malformed_and_projection.status == PGACCEL_OK &&
            malformed_and_projection.uncertain == std::vector<uint8_t>({1}) &&
            malformed_and_projection.values[0].tag == PGACCEL_VAL_NULL);
  CHECK("basic AND underflow predicate is uncertain",
        predicate(malformed_and, one.abi) == std::vector<int8_t>({PGACCEL_EXPR_UNCERTAIN}));

  std::vector<pgaccel_expr_instruction> excessive_pushes(65,
                                                         instruction(PGACCEL_EXPR_OP_LOAD_COL, 0));
  Program stack_overflow(std::move(excessive_pushes), {}, 1);
  const Projection stack_overflow_projection = project(stack_overflow, one.abi);
  CHECK("basic stack overflow is uncertain",
        stack_overflow_projection.status == PGACCEL_OK &&
            stack_overflow_projection.uncertain == std::vector<uint8_t>({1}));
  CHECK("basic stack overflow predicate is uncertain",
        predicate(stack_overflow, one.abi) == std::vector<int8_t>({PGACCEL_EXPR_UNCERTAIN}));
}

void test_column_tags_and_missing_values() {
  bool bools[] = {true};
  int32_t i32s[] = {-3};
  int64_t i64s[] = {INT64_C(9007199254740991)};
  float f32s[] = {1.25f};
  double f64s[] = {-2.5};
  int32_t dates[] = {8766};
  int64_t timestamps[] = {INT64_C(9007199254740993)};
  void* data[] = {bools, i32s, i64s, f32s, f64s, dates, timestamps};
  uint8_t* nulls[] = {nullptr, nullptr, nullptr, nullptr, nullptr, nullptr, nullptr};
  pgaccel_val_tag types[] = {PGACCEL_VAL_BOOL,     PGACCEL_VAL_INT32,   PGACCEL_VAL_INT64,
                             PGACCEL_VAL_FLOAT32,  PGACCEL_VAL_FLOAT64, PGACCEL_VAL_DATE,
                             PGACCEL_VAL_TIMESTAMP};
  pgaccel_batch batch{1, 7, data, nulls, types};

  ProgramBuilder checks;
  checks.begin_checks();
  const std::array<pgaccel_val, 7> expected = {bool_value(true),
                                               i32_value(-3),
                                               i64_value(INT64_C(9007199254740991)),
                                               f32_value(1.25f),
                                               f64_value(-2.5),
                                               date_value(8766),
                                               timestamp_value(INT64_C(9007199254740993))};
  for (uint32_t column = 0; column < expected.size(); ++column) {
    checks.op(PGACCEL_EXPR_OP_LOAD_COL, column);
    checks.push(expected[column]);
    checks.op(PGACCEL_EXPR_OP_EQ);
    checks.op(PGACCEL_EXPR_OP_AND);
  }
  ProgramBuilder extended_checks = checks;
  force_extended_tier(extended_checks);
  expect_check_program("all fixed-width column tags in extended tier", extended_checks, batch);
  expect_check_program("all fixed-width column tags", checks, batch);

  Program load_date({instruction(PGACCEL_EXPR_OP_LOAD_COL, 5)}, {}, 7);
  Projection result = project(load_date, batch);
  CHECK("DATE projection preserves tag", result.status == PGACCEL_OK &&
                                             result.values[0].tag == PGACCEL_VAL_DATE &&
                                             result.values[0].data.i32 == dates[0]);

  Program load_timestamp({instruction(PGACCEL_EXPR_OP_LOAD_COL, 6)}, {}, 7);
  result = project(load_timestamp, batch);
  CHECK("TIMESTAMP projection preserves tag and exact value",
        result.status == PGACCEL_OK && result.values[0].tag == PGACCEL_VAL_TIMESTAMP &&
            result.values[0].data.i64 == timestamps[0]);

  int32_t unknown_storage[] = {1};
  void* missing_data[] = {nullptr, unknown_storage};
  pgaccel_val_tag missing_types[] = {PGACCEL_VAL_INT32, static_cast<pgaccel_val_tag>(999)};
  pgaccel_batch missing{1, 2, missing_data, nullptr, missing_types};
  ProgramBuilder missing_checks;
  missing_checks.begin_checks();
  for (const uint32_t column : {0U, 1U, 8U}) {
    missing_checks.op(PGACCEL_EXPR_OP_LOAD_COL, column);
    missing_checks.op(PGACCEL_EXPR_OP_IS_NULL);
    missing_checks.op(PGACCEL_EXPR_OP_AND);
  }
  missing_checks.op(PGACCEL_EXPR_OP_LOAD_NULL);
  missing_checks.op(PGACCEL_EXPR_OP_IS_NULL);
  missing_checks.op(PGACCEL_EXPR_OP_AND);
  missing_checks.op(PGACCEL_EXPR_OP_LOAD_CONST, UINT32_MAX);
  missing_checks.op(PGACCEL_EXPR_OP_IS_NULL);
  missing_checks.op(PGACCEL_EXPR_OP_AND);
  ProgramBuilder extended_missing_checks = missing_checks;
  force_extended_tier(extended_missing_checks);
  expect_check_program("missing and invalid values become NULL in extended tier",
                       extended_missing_checks, missing);
  expect_check_program("missing and invalid values become NULL", missing_checks, missing);

  pgaccel_batch no_columns{1, 0, nullptr, nullptr, nullptr};
  Program load_null({instruction(PGACCEL_EXPR_OP_LOAD_NULL)}, {}, 0);
  result = project(load_null, no_columns);
  CHECK("LOAD_NULL with zero-column batch",
        result.status == PGACCEL_OK && result.values[0].tag == PGACCEL_VAL_NULL);
}

void test_control_flow_and_predicate_result_classes() {
  OneColumnBatch<int32_t> one({1}, PGACCEL_VAL_INT32);
  bool conditions[] = {true, false, false};
  uint8_t condition_nulls[] = {0, 0, 1};
  void* condition_data[] = {conditions};
  uint8_t* null_masks[] = {condition_nulls};
  pgaccel_val_tag condition_types[] = {PGACCEL_VAL_BOOL};
  pgaccel_batch condition_batch{3, 1, condition_data, null_masks, condition_types};
  Program branch({instruction(PGACCEL_EXPR_OP_LOAD_COL, 0),
                  instruction(PGACCEL_EXPR_OP_JUMP_IF_FALSE, 4),
                  instruction(PGACCEL_EXPR_OP_LOAD_CONST, 0), instruction(PGACCEL_EXPR_OP_JUMP, 5),
                  instruction(PGACCEL_EXPR_OP_LOAD_CONST, 1)},
                 {i32_value(10), i32_value(20)}, 1);
  Projection branch_result = project(branch, condition_batch);
  CHECK("CASE control flow status", branch_result.status == PGACCEL_OK &&
                                        branch_result.uncertain == std::vector<uint8_t>({0, 0, 0}));
  CHECK("CASE control flow values", branch_result.values[0].data.i32 == 10 &&
                                        branch_result.values[1].data.i32 == 20 &&
                                        branch_result.values[2].data.i32 == 20);

  ProgramBuilder extended_branch_builder;
  extended_branch_builder.op(PGACCEL_EXPR_OP_LOAD_COL, 0);
  extended_branch_builder.op(PGACCEL_EXPR_OP_JUMP_IF_FALSE, 4);
  extended_branch_builder.push(i32_value(10));
  extended_branch_builder.op(PGACCEL_EXPR_OP_JUMP, 5);
  extended_branch_builder.push(i32_value(20));
  force_extended_tier(extended_branch_builder);
  Program extended_branch(std::move(extended_branch_builder.instructions),
                          std::move(extended_branch_builder.constants), 1);
  branch_result = project(extended_branch, condition_batch);
  CHECK("extended CASE control flow status",
        branch_result.status == PGACCEL_OK &&
            branch_result.uncertain == std::vector<uint8_t>({0, 0, 0}));
  CHECK("extended CASE control flow values", branch_result.values[0].data.i32 == 10 &&
                                                 branch_result.values[1].data.i32 == 20 &&
                                                 branch_result.values[2].data.i32 == 20);

  Program always_true({instruction(PGACCEL_EXPR_OP_ALWAYS_TRUE)}, {}, 1);
  Projection projected = project(always_true, one.abi);
  CHECK("ALWAYS_TRUE projection", projected.status == PGACCEL_OK && projected.uncertain[0] == 0 &&
                                      projected.values[0].tag == PGACCEL_VAL_BOOL &&
                                      projected.values[0].data.b);
  CHECK("ALWAYS_TRUE predicate",
        predicate(always_true, one.abi) == std::vector<int8_t>({PGACCEL_EXPR_TRUE}));

  ProgramBuilder extended_always_true_builder;
  extended_always_true_builder.op(PGACCEL_EXPR_OP_ALWAYS_TRUE);
  force_extended_tier(extended_always_true_builder);
  Program extended_always_true(std::move(extended_always_true_builder.instructions),
                               std::move(extended_always_true_builder.constants), 1);
  projected = project(extended_always_true, one.abi);
  CHECK("extended ALWAYS_TRUE projection",
        projected.status == PGACCEL_OK && projected.uncertain[0] == 0 &&
            projected.values[0].tag == PGACCEL_VAL_BOOL && projected.values[0].data.b);
  CHECK("extended ALWAYS_TRUE predicate",
        predicate(extended_always_true, one.abi) == std::vector<int8_t>({PGACCEL_EXPR_TRUE}));

  Program unknown({instruction(PGACCEL_EXPR_OP_LOAD_CONST, 0), instruction(UINT16_MAX)},
                  {bool_value(true)}, 1);
  projected = project(unknown, one.abi);
  CHECK("unknown opcode is uncertain",
        projected.status == PGACCEL_OK && projected.uncertain[0] == 1 &&
            projected.values[0].tag == PGACCEL_VAL_BOOL && projected.values[0].data.b);
  CHECK("uncertain predicate class",
        predicate(unknown, one.abi) == std::vector<int8_t>({PGACCEL_EXPR_UNCERTAIN}));

  Program null_predicate({instruction(PGACCEL_EXPR_OP_LOAD_NULL)}, {}, 1);
  CHECK("NULL predicate is false",
        predicate(null_predicate, one.abi) == std::vector<int8_t>({PGACCEL_EXPR_FALSE}));

  OneColumnBatch<int32_t> i32_batch({0, -2}, PGACCEL_VAL_INT32);
  Program raw_i32({instruction(PGACCEL_EXPR_OP_LOAD_COL, 0)}, {}, 1);
  CHECK("i32 predicate conversion",
        predicate(raw_i32, i32_batch.abi) ==
            std::vector<int8_t>({PGACCEL_EXPR_FALSE, PGACCEL_EXPR_TRUE}));

  OneColumnBatch<int64_t> i64_batch({0, 9}, PGACCEL_VAL_INT64);
  Program raw_i64({instruction(PGACCEL_EXPR_OP_LOAD_COL, 0)}, {}, 1);
  CHECK("i64 predicate conversion",
        predicate(raw_i64, i64_batch.abi) ==
            std::vector<int8_t>({PGACCEL_EXPR_FALSE, PGACCEL_EXPR_TRUE}));
}

void test_public_argument_contracts() {
  OneColumnBatch<int32_t> batch({1}, PGACCEL_VAL_INT32);
  Program program({instruction(PGACCEL_EXPR_OP_ALWAYS_TRUE)}, {}, 1);
  int8_t predicate_result = 99;
  pgaccel_val output{};
  uint8_t uncertain = 99;

  CHECK("predicate rejects null program",
        pgaccel_expr_eval_predicate(nullptr, &batch.abi, &predicate_result) == PGACCEL_ERROR);
  CHECK("predicate rejects null batch",
        pgaccel_expr_eval_predicate(&program.abi, nullptr, &predicate_result) == PGACCEL_ERROR);
  CHECK("predicate rejects null output",
        pgaccel_expr_eval_predicate(&program.abi, &batch.abi, nullptr) == PGACCEL_ERROR);
  CHECK("project rejects null program",
        pgaccel_expr_eval_project(nullptr, &batch.abi, &output, &uncertain) == PGACCEL_ERROR);
  CHECK("project rejects null batch",
        pgaccel_expr_eval_project(&program.abi, nullptr, &output, &uncertain) == PGACCEL_ERROR);
  CHECK("project rejects null output",
        pgaccel_expr_eval_project(&program.abi, &batch.abi, nullptr, &uncertain) == PGACCEL_ERROR);

  pgaccel_expr_program invalid_program = program.abi;
  invalid_program.instructions = nullptr;
  CHECK("predicate rejects missing instruction storage",
        pgaccel_expr_eval_predicate(&invalid_program, &batch.abi, &predicate_result) ==
            PGACCEL_ERROR);
  CHECK("project rejects missing instruction storage",
        pgaccel_expr_eval_project(&invalid_program, &batch.abi, &output, &uncertain) ==
            PGACCEL_ERROR);

  invalid_program = program.abi;
  invalid_program.const_count = 1;
  invalid_program.const_pool = nullptr;
  CHECK("predicate rejects missing constant storage",
        pgaccel_expr_eval_predicate(&invalid_program, &batch.abi, &predicate_result) ==
            PGACCEL_ERROR);
  CHECK("project rejects missing constant storage",
        pgaccel_expr_eval_project(&invalid_program, &batch.abi, &output, &uncertain) ==
            PGACCEL_ERROR);

  pgaccel_batch invalid_batch = batch.abi;
  invalid_batch.col_data = nullptr;
  CHECK("predicate rejects missing column data array",
        pgaccel_expr_eval_predicate(&program.abi, &invalid_batch, &predicate_result) ==
            PGACCEL_ERROR);
  CHECK("project rejects missing column data array",
        pgaccel_expr_eval_project(&program.abi, &invalid_batch, &output, &uncertain) ==
            PGACCEL_ERROR);

  invalid_batch = batch.abi;
  invalid_batch.col_types = nullptr;
  CHECK("predicate rejects missing column type array",
        pgaccel_expr_eval_predicate(&program.abi, &invalid_batch, &predicate_result) ==
            PGACCEL_ERROR);
  CHECK("project rejects missing column type array",
        pgaccel_expr_eval_project(&program.abi, &invalid_batch, &output, &uncertain) ==
            PGACCEL_ERROR);

  CHECK("project permits null uncertain mask",
        pgaccel_expr_eval_project(&program.abi, &batch.abi, &output, nullptr) == PGACCEL_OK &&
            output.tag == PGACCEL_VAL_BOOL && output.data.b);

  pgaccel_batch empty{0, 0, nullptr, nullptr, nullptr};
  CHECK("empty predicate is a no-op",
        pgaccel_expr_eval_predicate(&program.abi, &empty, &predicate_result) == PGACCEL_OK &&
            predicate_result == 99);
  CHECK("empty project is a no-op",
        pgaccel_expr_eval_project(&program.abi, &empty, &output, &uncertain) == PGACCEL_OK &&
            uncertain == 99);
}

void expect_bytecode_rejected(const char* case_name, Program& program, pgaccel_batch& batch) {
  const uint64_t executions_before = pgaccel_gpu_exec_count();
  int8_t predicate_result = 99;
  pgaccel_val output = i32_value(99);
  uint8_t uncertain = 99;
  char label[160];

  std::snprintf(label, sizeof(label), "%s predicate rejected", case_name);
  CHECK(label,
        pgaccel_expr_eval_predicate(&program.abi, &batch, &predicate_result) == PGACCEL_ERROR);
  std::snprintf(label, sizeof(label), "%s project rejected", case_name);
  CHECK(label,
        pgaccel_expr_eval_project(&program.abi, &batch, &output, &uncertain) == PGACCEL_ERROR);
  std::snprintf(label, sizeof(label), "%s does not dispatch", case_name);
  CHECK(label, pgaccel_gpu_exec_count() == executions_before);
}

void test_common_extended_bytecode_validation() {
  pgaccel_batch batch{1, 0, nullptr, nullptr, nullptr};

  Program common_underflow({instruction(PGACCEL_EXPR_OP_ADD_I32)}, {}, 0);
  expect_bytecode_rejected("common stack underflow", common_underflow, batch);

  Program extended_underflow({instruction(PGACCEL_EXPR_OP_POW_F64)}, {}, 0);
  expect_bytecode_rejected("extended stack underflow", extended_underflow, batch);

  std::vector<pgaccel_expr_instruction> pushes(65, instruction(PGACCEL_EXPR_OP_LOAD_NULL));
  Program common_overflow(std::move(pushes), {}, 0);
  expect_bytecode_rejected("common stack overflow", common_overflow, batch);

  Program zero_jump({instruction(PGACCEL_EXPR_OP_JUMP, 0)}, {}, 0);
  expect_bytecode_rejected("zero jump", zero_jump, batch);

  Program backward_jump(
      {instruction(PGACCEL_EXPR_OP_LOAD_NULL), instruction(PGACCEL_EXPR_OP_JUMP, 0)}, {}, 0);
  expect_bytecode_rejected("backward jump", backward_jump, batch);

  Program self_jump({instruction(PGACCEL_EXPR_OP_LOAD_NULL), instruction(PGACCEL_EXPR_OP_JUMP, 1)},
                    {}, 0);
  expect_bytecode_rejected("self jump", self_jump, batch);

  Program out_of_range_jump({instruction(PGACCEL_EXPR_OP_JUMP, 2)}, {}, 0);
  expect_bytecode_rejected("out-of-range jump", out_of_range_jump, batch);

  Program inconsistent_merge(
      {instruction(PGACCEL_EXPR_OP_ALWAYS_TRUE), instruction(PGACCEL_EXPR_OP_JUMP_IF_FALSE, 4),
       instruction(PGACCEL_EXPR_OP_LOAD_NULL), instruction(PGACCEL_EXPR_OP_JUMP, 5),
       instruction(PGACCEL_EXPR_OP_JUMP, 5)},
      {}, 0);
  expect_bytecode_rejected("inconsistent stack merge", inconsistent_merge, batch);
}

void test_no_device_paths() {
  const pgaccel_status init_status = pgaccel_init();
  CHECK("CPU-only visibility has no GPU device", init_status != PGACCEL_OK);
  if (init_status == PGACCEL_OK) {
    pgaccel_shutdown();
    return;
  }

  pgaccel_batch batch{1, 0, nullptr, nullptr, nullptr};
  Program program({instruction(PGACCEL_EXPR_OP_ALWAYS_TRUE)}, {}, 0);
  int8_t predicate_result = 99;
  pgaccel_val output = i32_value(99);
  uint8_t uncertain = 99;

  CHECK("predicate reports no device",
        pgaccel_expr_eval_predicate(&program.abi, &batch, &predicate_result) ==
            PGACCEL_ERROR_NO_DEVICE);
  CHECK("no-device predicate result is uncertain", predicate_result == PGACCEL_EXPR_UNCERTAIN);
  CHECK("project reports no device",
        pgaccel_expr_eval_project(&program.abi, &batch, &output, &uncertain) ==
            PGACCEL_ERROR_NO_DEVICE);
  CHECK("failed no-device initialization shuts down", pgaccel_shutdown() == PGACCEL_OK);
}

bool run_no_device_child(const char* executable) {
  const pid_t child = fork();
  if (child < 0) {
    std::fprintf(stderr, "FAIL: fork no-device expr VM matrix: errno=%d\n", errno);
    return false;
  }
  if (child == 0) {
    const char* visibility_mask = std::getenv("PGACCEL_TEST_NO_DEVICE_MASK");
    setenv("ACPP_VISIBILITY_MASK", visibility_mask != nullptr ? visibility_mask : "cuda", 1);
    setenv("PGACCEL_TEST_NO_DEVICE", "1", 1);
    execl(executable, executable, static_cast<char*>(nullptr));
    std::fprintf(stderr, "FAIL: exec no-device expr VM matrix: errno=%d\n", errno);
    _exit(127);
  }

  int status = 0;
  pid_t waited;
  do {
    waited = waitpid(child, &status, 0);
  } while (waited < 0 && errno == EINTR);
  if (waited != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
    std::fprintf(stderr, "FAIL: no-device expr VM matrix child: status=%d errno=%d\n", status,
                 errno);
    return false;
  }
  return true;
}

}  // namespace

int main(int argc, char** argv) {
  if (std::getenv("PGACCEL_TEST_NO_DEVICE") != nullptr) {
    test_no_device_paths();
    std::printf("test_expr_vm_matrix no-device: %zu check(s)\n", check_count);
    std::printf("test_expr_vm_matrix no-device: %d failure(s)\n", failures);
    return failures == 0 ? 0 : 1;
  }

  CHECK("no-device expr VM matrix child",
        argc > 0 && argv[0] != nullptr && run_no_device_child(argv[0]));
  if (pgaccel_init() != PGACCEL_OK) {
    std::fprintf(stderr, "FAIL: pgaccel_init\n");
    return 1;
  }
  pgaccel_reset_gpu_exec_count();

  test_integer_boundaries();
  test_compact_arithmetic_matrix();
  test_arithmetic_null_and_error_matrix();
  test_comparisons_and_predicates();
  test_comparison_matrix();
  test_boolean_cast_and_math_matrix();
  test_round();
  test_extended_math_dispatch_split();
  test_basic_expression_tier();
  test_column_tags_and_missing_values();
  test_control_flow_and_predicate_result_classes();
  test_public_argument_contracts();
  test_common_extended_bytecode_validation();

  CHECK("GPU execution counter", pgaccel_gpu_exec_count() > 0);
  pgaccel_shutdown();
  std::printf("test_expr_vm_matrix: %zu check(s)\n", check_count);
  std::printf("test_expr_vm_matrix: %d failure(s)\n", failures);
  return failures == 0 ? 0 : 1;
}
