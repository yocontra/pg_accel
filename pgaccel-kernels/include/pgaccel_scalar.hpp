#pragma once

#include <cstddef>
#include <cstdint>

// Internal SPG1 device evaluator, not an exported C ABI or a planner admission.
// The binding layer must first validate the ENTIRE program with ScalarProgram
// or validate_program below (including inactive branches). Runtime checks defend executed accesses;
// they are not a substitute for the host's control-flow/type proof.
namespace pgaccel_scalar {

constexpr int32_t MAGIC = 0x53504731;
constexpr size_t MAX_INPUTS = 64;
constexpr size_t MAX_INSTRUCTIONS = 256;
constexpr size_t MAX_STACK = 32;
constexpr size_t MAX_WORDS = 3 + MAX_INPUTS + 2 * MAX_INSTRUCTIONS;
constexpr int32_t BOOL = 1;
constexpr int32_t INT4 = 2;
constexpr int32_t DATE = 6;
constexpr int32_t DATE_MIN = -2451545;
constexpr int32_t DATE_END = 2145031949;

enum class Status : int32_t {
  Ok = 0, Overflow = 1, DivisionByZero = 2, Invalid = 3, DateOutOfRange = 4
};

inline bool valid_date(int32_t value) {
  return value == INT32_MIN || value == INT32_MAX || (value >= DATE_MIN && value < DATE_END);
}

// NULL retains its type. Boolean payloads must be canonical 0/1. NULL payloads
// are normalized to zero, so no uninitialized data can enter an operation.
struct Value {
  int32_t type;
  int32_t payload;
  int32_t is_null;
};

struct Result {
  Status status;
  Value value;
};

inline bool valid_type(int32_t type) {
  return type == BOOL || type == INT4 || type == DATE;
}

inline bool valid_value(Value value) {
  return valid_type(value.type) && (value.is_null == 0 || value.is_null == 1) &&
         (value.is_null ? value.payload == 0
                        : value.type == DATE ? valid_date(value.payload)
                        : value.type == INT4 || value.payload == 0 || value.payload == 1);
}

inline Result fault(Status status) {
  return {status, {INT4, 0, 1}};
}

// Shared by ordinary, immediate and coalesced-RHS arithmetic. Type checking
// precedes NULL propagation; zero divisors are errors only for non-NULL pairs.
inline Result arithmetic(int32_t op, Value lhs, Value rhs) {
  if (op < 10 || op > 14 || lhs.type != INT4 || rhs.type != INT4)
    return fault(Status::Invalid);
  if (lhs.is_null || rhs.is_null)
    return {Status::Ok, {INT4, 0, 1}};
  const int32_t a = lhs.payload, b = rhs.payload;
  int64_t wide = 0;
  switch (op) {
    case 10:
      wide = static_cast<int64_t>(a) + b;
      break;
    case 11:
      wide = static_cast<int64_t>(a) - b;
      break;
    case 12:
      wide = static_cast<int64_t>(a) * b;
      break;
    case 13:
    case 14:
      if (b == 0)
        return fault(Status::DivisionByZero);
      // Avoid signed division/remainder UB. PostgreSQL MIN % -1 is zero.
      if (a == INT32_MIN && b == -1) {
        if (op == 13)
          return fault(Status::Overflow);
      } else {
        wide = op == 13 ? a / b : a % b;
      }
      break;
  }
  if (wide < INT32_MIN || wide > INT32_MAX)
    return fault(Status::Overflow);
  return {Status::Ok, {INT4, static_cast<int32_t>(wide), 0}};
}

struct Proof {
  size_t input_count = 0;
  size_t instruction_count = 0;
  size_t max_stack = 0;
  int32_t result_type = 0;
  bool uses_predicate_mask = false;
};

// Host-side full CFG proof for the C descriptor boundary. Like Rust's
// ScalarProgram validator, this checks inactive edges, canonical operands,
// unreachable instructions and type/depth agreement at every merge. No row
// data is read and no expression is executed on the host.
inline bool validate_program(const int32_t* words, size_t word_count, Proof* proof) {
  if (proof)
    *proof = {};
  if (!words || word_count < 3 || word_count > MAX_WORDS || words[0] != MAGIC ||
      !valid_type(words[1]) || words[2] < 0 || static_cast<size_t>(words[2]) > MAX_INPUTS)
    return false;
  const size_t inputs = static_cast<size_t>(words[2]);
  const size_t body = 3 + inputs;
  if (word_count <= body || (word_count - body) % 2 != 0)
    return false;
  const size_t count = (word_count - body) / 2;
  if (count > MAX_INSTRUCTIONS)
    return false;
  for (size_t i = 0; i < inputs; ++i)
    if (!valid_type(words[3 + i]))
      return false;
  int16_t depths[MAX_INSTRUCTIONS + 1];
  uint8_t states[MAX_INSTRUCTIONS + 1][MAX_STACK]{};
  for (auto& depth : depths)
    depth = -1;
  depths[0] = 0;
  size_t max_stack = 0;
  bool uses_predicate_mask = false;
  const auto merge = [&](size_t target, const uint8_t* stack, size_t depth) {
    if (depths[target] >= 0) {
      if (static_cast<size_t>(depths[target]) != depth)
        return false;
      for (size_t i = 0; i < depth; ++i)
        if (states[target][i] != stack[i])
          return false;
    } else {
      depths[target] = static_cast<int16_t>(depth);
      for (size_t i = 0; i < depth; ++i)
        states[target][i] = stack[i];
    }
    return true;
  };
  for (size_t pc = 0; pc < count; ++pc) {
    if (depths[pc] < 0)
      return false;
    size_t depth = static_cast<size_t>(depths[pc]);
    uint8_t stack[MAX_STACK]{};
    for (size_t i = 0; i < depth; ++i)
      stack[i] = states[pc][i];
    const int32_t op = words[body + 2 * pc];
    const int32_t arg = words[body + 2 * pc + 1];
    if (op >= 50 && op <= 55) {
      if (arg < 0 || static_cast<size_t>(arg) <= pc || static_cast<size_t>(arg) > count)
        return false;
      if (op == 55) {
        if (depth < 2 || stack[depth - 1] != stack[depth - 2])
          return false;
        depth -= 2;
      } else if (op != 50) {
        if (depth == 0 || (op != 54 && stack[depth - 1] != BOOL))
          return false;
        if (op != 54)
          --depth;
      }
      if (!merge(static_cast<size_t>(arg), stack, depth))
        return false;
      if (op == 50)
        continue;
      if (op == 54)
        --depth;
    } else if ((op >= 0 && op <= 4) || op == 7 || op == 8) {
      if (depth == MAX_STACK)
        return false;
      int32_t type = 0;
      if (op == 0) {
        if (arg < 0 || static_cast<size_t>(arg) >= inputs)
          return false;
        type = words[3 + arg];
      } else if (op == 8) {
        if (arg != 0)
          return false;
        type = BOOL;
        uses_predicate_mask = true;
      } else if (op == 7) {
        if (!valid_date(arg))
          return false;
        type = DATE;
      } else if (op == 1)
        type = INT4;
      else if (op == 2) {
        if (arg != 0 && arg != 1)
          return false;
        type = BOOL;
      } else if (op == 3) {
        if (!valid_type(arg))
          return false;
        type = arg;
      } else {
        if (arg != 0 || depth == 0)
          return false;
        type = stack[depth - 1];
      }
      stack[depth++] = static_cast<uint8_t>(type);
    } else if (op == 6) {
      if (depth == 0 || stack[depth - 1] != INT4)
        return false;
    } else if (op >= 60 && op <= 69) {
      if (depth == 0 || stack[depth - 1] != INT4)
        return false;
      if (op >= 65) {
        if (depth < 2 || stack[depth - 2] != INT4)
          return false;
        --depth;
      }
    } else {
      if (arg != 0)
        return false;
      if (op >= 70 && op <= 73) {
        if (depth < 2 || stack[depth - 2] != (op == 72 ? INT4 : DATE) ||
            stack[depth - 1] != (op >= 72 ? DATE : INT4))
          return false;
        --depth;
        stack[depth - 1] = op == 73 ? INT4 : DATE;
      } else if (op == 5) {
        if (depth == 0)
          return false;
        --depth;
      } else if (op == 15 || op == 32 || op == 40 || op == 41) {
        if (depth == 0)
          return false;
        if (op == 15 || op == 32) {
          if (stack[depth - 1] != (op == 15 ? INT4 : BOOL))
            return false;
        } else
          stack[depth - 1] = BOOL;
      } else {
        const bool arithmetic = op >= 10 && op <= 14;
        const bool comparison = op >= 20 && op <= 26;
        const bool selection = op >= 27 && op <= 29;
        const bool boolean = op == 30 || op == 31;
        if ((!arithmetic && !comparison && !selection && !boolean) || depth < 2 ||
            stack[depth - 1] != stack[depth - 2] || (arithmetic && stack[depth - 1] != INT4) ||
            (boolean && stack[depth - 1] != BOOL))
          return false;
        --depth;
        if (!selection)
          stack[depth - 1] = arithmetic ? INT4 : BOOL;
      }
    }
    if (depth > max_stack)
      max_stack = depth;
    if (!merge(pc + 1, stack, depth))
      return false;
  }
  if (depths[count] != 1 || states[count][0] != words[1])
    return false;
  if (proof)
    *proof = {inputs, count, max_stack, words[1], uses_predicate_mask};
  return true;
}

// ReadInput(index) must return this row's typed value from resident input
// buffers. It is invoked only for an executed LOAD, preserving lazy branches.
// The wire and input buffers must be accessible to the current device.
template <class ReadInput>
inline Result evaluate(const int32_t* words, size_t word_count, size_t input_count,
                       ReadInput read_input, Value predicate_mask = {0, 0, 0}) {
  if (!words || word_count < 3 || words[0] != MAGIC || !valid_type(words[1]) || words[2] < 0 ||
      static_cast<size_t>(words[2]) != input_count || input_count > MAX_INPUTS ||
      word_count < 3 + input_count)
    return fault(Status::Invalid);
  const size_t body = 3 + input_count;
  const size_t body_words = word_count - body;
  if (body_words == 0 || body_words % 2 != 0 || body_words / 2 > MAX_INSTRUCTIONS)
    return fault(Status::Invalid);
  const size_t instruction_count = body_words / 2;
  Value stack[MAX_STACK];
  size_t depth = 0;
  size_t pc = 0;
  while (pc < instruction_count) {
    const int32_t op = words[body + 2 * pc];
    const int32_t arg = words[body + 2 * pc + 1];
    ++pc;
    if (op >= 50 && op <= 55) {
      // pc has already advanced: a target equal to pc is a legal forward edge.
      if (arg < 0 || static_cast<size_t>(arg) < pc || static_cast<size_t>(arg) > instruction_count)
        return fault(Status::Invalid);
      bool jump = op == 50;
      if (op == 55) {
        if (depth < 2 || stack[depth - 1].type != stack[depth - 2].type)
          return fault(Status::Invalid);
        const Value rhs = stack[--depth];
        const Value lhs = stack[--depth];
        jump = lhs.is_null || rhs.is_null || lhs.payload != rhs.payload;
      } else if (op != 50) {
        if (depth == 0)
          return fault(Status::Invalid);
        const Value value = stack[depth - 1];
        if (op == 54) {
          jump = !value.is_null;
          if (!jump)
            --depth;
        } else {
          if (value.type != BOOL)
            return fault(Status::Invalid);
          --depth;
          const bool is_true = !value.is_null && value.payload == 1;
          const bool is_false = !value.is_null && value.payload == 0;
          jump = op == 51 ? !is_true : op == 52 ? is_true : is_false;
        }
      }
      if (jump)
        pc = static_cast<size_t>(arg);
      continue;
    }
    if ((op >= 0 && op <= 4) || op == 7 || op == 8) {
      if (depth == MAX_STACK)
        return fault(Status::Invalid);
      Value value{INT4, 0, 1};
      switch (op) {
        case 0:
          if (arg < 0 || static_cast<size_t>(arg) >= input_count)
            return fault(Status::Invalid);
          value = read_input(static_cast<size_t>(arg));
          if (!valid_value(value) || value.type != words[3 + arg])
            return fault(Status::Invalid);
          break;
        case 1:
          value = {INT4, arg, 0};
          break;
        case 7:
          if (!valid_date(arg))
            return fault(Status::Invalid);
          value = {DATE, arg, 0};
          break;
        case 8:
          if (arg != 0 || predicate_mask.type != BOOL || !valid_value(predicate_mask))
            return fault(Status::Invalid);
          value = predicate_mask;
          break;
        case 2:
          if (arg != 0 && arg != 1)
            return fault(Status::Invalid);
          value = {BOOL, arg, 0};
          break;
        case 3:
          if (!valid_type(arg))
            return fault(Status::Invalid);
          value = {arg, 0, 1};
          break;
        case 4:
          if (arg != 0 || depth == 0)
            return fault(Status::Invalid);
          value = stack[depth - 1];
          break;
      }
      stack[depth++] = value;
      continue;
    }
    if (op == 6) {
      if (depth == 0 || stack[depth - 1].type != INT4)
        return fault(Status::Invalid);
      if (stack[depth - 1].is_null)
        stack[depth - 1] = {INT4, arg, 0};
      continue;
    }
    if (op >= 60 && op <= 69) {
      if (depth < (op < 65 ? 1u : 2u))
        return fault(Status::Invalid);
      Value rhs{INT4, arg, 0};
      if (op >= 65) {
        rhs = stack[--depth];
        if (rhs.type != INT4)
          return fault(Status::Invalid);
        if (rhs.is_null)
          rhs = {INT4, arg, 0};
      }
      const Result result = arithmetic(10 + (op - 60) % 5, stack[depth - 1], rhs);
      if (result.status != Status::Ok)
        return result;
      stack[depth - 1] = result.value;
      continue;
    }
    if (arg != 0)
      return fault(Status::Invalid);
    if (op >= 70 && op <= 73) {
      if (depth < 2 || stack[depth - 2].type != (op == 72 ? INT4 : DATE) ||
          stack[depth - 1].type != (op >= 72 ? DATE : INT4))
        return fault(Status::Invalid);
      const Value rhs = stack[--depth];
      const Value lhs = stack[depth - 1];
      const int32_t result_type = op == 73 ? INT4 : DATE;
      if (lhs.is_null || rhs.is_null) {
        stack[depth - 1] = {result_type, 0, 1};
        continue;
      }
      const int32_t date = op == 72 ? rhs.payload : lhs.payload;
      const int32_t days = op == 72 ? lhs.payload : rhs.payload;
      const bool infinite = date == INT32_MIN || date == INT32_MAX;
      if (op == 73) {
        if (infinite || days == INT32_MIN || days == INT32_MAX)
          return fault(Status::DateOutOfRange);
        // The maximum finite DATE span fits INT4, unlike arbitrary INT4 subtraction.
        stack[depth - 1] = {INT4, static_cast<int32_t>(static_cast<int64_t>(date) - days), 0};
      } else if (infinite) {
        stack[depth - 1] = {DATE, date, 0};
      } else {
        const int64_t result = op == 71 ? static_cast<int64_t>(date) - days
                                        : static_cast<int64_t>(date) + days;
        if (result < DATE_MIN || result >= DATE_END)
          return fault(Status::DateOutOfRange);
        stack[depth - 1] = {DATE, static_cast<int32_t>(result), 0};
      }
      continue;
    }
    if (op == 5) {
      if (depth == 0)
        return fault(Status::Invalid);
      --depth;
      continue;
    }
    if (op == 15 || op == 32 || op == 40 || op == 41) {
      if (depth == 0)
        return fault(Status::Invalid);
      Value& value = stack[depth - 1];
      if (op == 40 || op == 41) {
        value = {BOOL, op == 40 ? value.is_null : !value.is_null, 0};
      } else {
        if (value.type != (op == 15 ? INT4 : BOOL))
          return fault(Status::Invalid);
        if (!value.is_null) {
          if (op == 15 && value.payload == INT32_MIN)
            return fault(Status::Overflow);
          value.payload = op == 15 ? -value.payload : !value.payload;
        }
      }
      continue;
    }
    const bool arithmetic = op >= 10 && op <= 14;
    const bool comparison = op >= 20 && op <= 26;
    const bool selection = op >= 27 && op <= 29;
    const bool boolean = op == 30 || op == 31;
    if ((!arithmetic && !comparison && !selection && !boolean) || depth < 2)
      return fault(Status::Invalid);
    const Value rhs = stack[--depth];
    const Value lhs = stack[depth - 1];
    if (arithmetic) {
      const Result result = pgaccel_scalar::arithmetic(op, lhs, rhs);
      if (result.status != Status::Ok)
        return result;
      stack[depth - 1] = result.value;
      continue;
    }
    if (lhs.type != rhs.type || (arithmetic && lhs.type != INT4) || (boolean && lhs.type != BOOL))
      return fault(Status::Invalid);
    if (op == 26) {
      // Both arguments have already executed, including any typed failures.
      // Payloads of NULL values never participate in the comparison.
      stack[depth - 1] = {BOOL, lhs.is_null != rhs.is_null ||
                                   (!lhs.is_null && lhs.payload != rhs.payload),
                          0};
      continue;
    }
    if (selection) {
      // Arguments execute before selection: neither a NULL nor an extremal
      // earlier value may suppress a later operand's typed error.
      if (op == 27)
        stack[depth - 1] = !lhs.is_null && !rhs.is_null && lhs.payload == rhs.payload
                               ? Value{lhs.type, 0, 1} : lhs;
      else if (lhs.is_null)
        stack[depth - 1] = rhs;
      else if (rhs.is_null)
        stack[depth - 1] = lhs;
      else
        stack[depth - 1] = (op == 28 ? lhs.payload < rhs.payload : lhs.payload > rhs.payload)
                               ? rhs : lhs;
      continue;
    }
    Value value{arithmetic ? INT4 : BOOL, 0, lhs.is_null || rhs.is_null};
    if (boolean) {
      const bool a_true = !lhs.is_null && lhs.payload == 1;
      const bool b_true = !rhs.is_null && rhs.payload == 1;
      const bool a_false = !lhs.is_null && lhs.payload == 0;
      const bool b_false = !rhs.is_null && rhs.payload == 0;
      if (op == 30 && (a_false || b_false))
        value = {BOOL, 0, 0};
      else if (op == 31 && (a_true || b_true))
        value = {BOOL, 1, 0};
      else if (!value.is_null)
        value.payload = op == 30;
    } else if (!value.is_null) {
      const int32_t a = lhs.payload;
      const int32_t b = rhs.payload;
      if (comparison) {
        switch (op) {
          case 20:
            value.payload = a == b;
            break;
          case 21:
            value.payload = a != b;
            break;
          case 22:
            value.payload = a < b;
            break;
          case 23:
            value.payload = a <= b;
            break;
          case 24:
            value.payload = a > b;
            break;
          case 25:
            value.payload = a >= b;
            break;
        }
      }
    }
    stack[depth - 1] = value;
  }
  if (depth != 1 || stack[0].type != words[1])
    return fault(Status::Invalid);
  return {Status::Ok, stack[0]};
}

}  // namespace pgaccel_scalar
