#include <sycl/sycl.hpp>

#include <array>
#include <cstdio>
#include <fstream>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>

#include "pgaccel_ffi.h"
#include "pgaccel_scalar.hpp"

extern sycl::queue* g_queue;
namespace s = pgaccel_scalar;

namespace {
struct Case {
  std::string name;
  std::vector<int32_t> words;
  std::vector<s::Value> inputs;
  s::Result expected;
  s::Value mask{0, 0, 0};
};
struct Binding {
  size_t word_offset, word_count, input_offset, input_count;
  s::Value mask;
};

s::Value integer(int32_t value) {
  return {s::INT4, value, 0};
}
s::Value boolean(bool value) {
  return {s::BOOL, value, 0};
}
s::Value null(int32_t type) {
  return {type, 0, 1};
}
s::Result ok(s::Value value) {
  return {s::Status::Ok, value};
}

std::vector<int32_t> numbers(const std::string& text) {
  std::istringstream input(text);
  std::vector<int32_t> values;
  int32_t value;
  while (input >> value)
    values.push_back(value);
  if (!input.eof())
    throw std::runtime_error("malformed fixture integer");
  return values;
}

std::vector<Case> corpus() {
  std::ifstream input(PGACCEL_SCALAR_CASES);
  if (!input)
    throw std::runtime_error("missing shared scalar corpus");
  std::vector<Case> cases;
  std::string line;
  while (std::getline(input, line)) {
    if (line.empty() || line[0] == '#')
      continue;
    std::istringstream fields(line);
    std::array<std::string, 4> parts;
    for (auto& part : parts)
      if (!std::getline(fields, part, '|'))
        throw std::runtime_error("truncated scalar fixture");
    std::string extra;
    if (std::getline(fields, extra, '|'))
      throw std::runtime_error("trailing scalar fixture");
    Case test{parts[0], numbers(parts[1]), {}, {}};
    auto inputs = numbers(parts[2]);
    auto expected = numbers(parts[3]);
    if (inputs.size() % 3 || expected.size() != 4)
      throw std::runtime_error("invalid scalar fixture shape");
    for (size_t i = 0; i < inputs.size(); i += 3)
      test.inputs.push_back({inputs[i], inputs[i + 1], inputs[i + 2]});
    test.expected = {static_cast<s::Status>(expected[0]), {expected[1], expected[2], expected[3]}};
    cases.push_back(test);
  }
  if (cases.size() != 468)
    throw std::runtime_error("shared scalar corpus lost cases");
  return cases;
}

void boundary_cases(std::vector<Case>& cases) {
  const int32_t values[] = {INT32_MIN, INT32_MIN + 1, -65536, -46341,        -1,       0,
                            1,         46341,         65536,  INT32_MAX - 1, INT32_MAX};
  // Independent widened host arithmetic oracle, never used for production.
  for (int32_t op = 10; op <= 14; ++op) {
    for (int32_t a : values) {
      for (int32_t b : values) {
        s::Result expected;
        if ((op == 13 || op == 14) && b == 0) {
          expected = s::fault(s::Status::DivisionByZero);
        } else {
          const int64_t x = a, y = b;
          int64_t wide = 0;
          if (op == 10)
            wide = x + y;
          if (op == 11)
            wide = x - y;
          if (op == 12)
            wide = x * y;
          if (op == 13)
            wide = x / y;
          if (op == 14)
            wide = x % y;
          expected = wide < INT32_MIN || wide > INT32_MAX ? s::fault(s::Status::Overflow)
                                                          : ok(integer(static_cast<int32_t>(wide)));
        }
        cases.push_back({"arithmetic boundary",
                         {s::MAGIC, s::INT4, 2, s::INT4, s::INT4, 0, 0, 0, 1, op, 0},
                         {integer(a), integer(b)},
                         expected});
        for (bool coalesced : {false, true}) {
          const int32_t fused = op + (coalesced ? 55 : 50);
          std::vector<int32_t> words{s::MAGIC, s::INT4, coalesced ? 2 : 1, s::INT4};
          if (coalesced)
            words.push_back(s::INT4);
          words.insert(words.end(), {0, 0});
          if (coalesced)
            words.insert(words.end(), {0, 1});
          words.insert(words.end(), {fused, b});
          if (!s::validate_program(words.data(), words.size(), nullptr))
            throw std::runtime_error("fused arithmetic CFG rejected");
          cases.push_back({"fused arithmetic boundary", words,
                           coalesced ? std::vector<s::Value>{integer(a), null(s::INT4)}
                                     : std::vector<s::Value>{integer(a)}, expected});
          cases.push_back({"fused arithmetic NULL lhs", words,
                           coalesced ? std::vector<s::Value>{null(s::INT4), null(s::INT4)}
                                     : std::vector<s::Value>{null(s::INT4)}, ok(null(s::INT4))});
          if (coalesced) {
            // A non-NULL RHS must override the fallback, including zero.
            words.back() = 42;
            cases.push_back({"fused arithmetic retained RHS", words,
                             {integer(a), integer(b)}, expected});
          }
        }
      }
    }
  }
  std::vector<int32_t> words{s::MAGIC, s::INT4, 0};
  for (size_t i = 0; i < s::MAX_STACK; ++i)
    words.insert(words.end(), {1, 1});
  for (size_t i = 1; i < s::MAX_STACK; ++i)
    words.insert(words.end(), {10, 0});
  cases.push_back({"exact stack limit", words, {}, ok(integer(32))});
  words.insert(words.begin() + 3, {1, 1});
  cases.push_back({"stack overflow", words, {}, s::fault(s::Status::Invalid)});
  words = {s::MAGIC, s::INT4, 0, 1, 1};
  for (size_t i = 1; i < s::MAX_INSTRUCTIONS; ++i)
    words.insert(words.end(), {15, 0});
  cases.push_back({"exact instruction limit", words, {}, ok(integer(-1))});
  words.insert(words.end(), {15, 0});
  cases.push_back({"instruction overflow", words, {}, s::fault(s::Status::Invalid)});
  words = {s::MAGIC, s::INT4, 64};
  words.insert(words.end(), 64, s::INT4);
  words.insert(words.end(), {0, 63});
  cases.push_back(
      {"exact input limit", words, std::vector<s::Value>(64, integer(42)), ok(integer(42))});
  words[2] = 65;
  words.insert(words.begin() + 3, s::INT4);
  cases.push_back({"input overflow", words, std::vector<s::Value>(65, integer(42)),
                   s::fault(s::Status::Invalid)});

  // Inactive inputs must not be read: these bindings are deliberately invalid.
  // The host contract still validates both paths' program types before launch.
  cases.push_back({"CASE skips input",
                   {s::MAGIC, s::INT4, 1, s::INT4, 2, 0, 51, 4, 0, 0, 50, 5, 1, 42},
                   {{99, 0, 0}},
                   ok(integer(42))});
  cases.push_back({"COALESCE skips input",
                   {s::MAGIC, s::INT4, 1, s::INT4, 1, 42, 54, 3, 0, 0},
                   {{99, 0, 0}},
                   ok(integer(42))});
  for (int32_t op : {30, 31}) {
    const bool short_value = op == 31;
    cases.push_back(
        {"Boolean skips input",
         {s::MAGIC, s::BOOL, 1, s::BOOL, 2, short_value, 4, 0, op == 30 ? 53 : 52, 5, 0, 0, op, 0},
         {{99, 0, 0}},
         ok(boolean(short_value))});
    cases.push_back(
        {"NULL must evaluate rhs",
         {s::MAGIC, s::BOOL, 1, s::BOOL, 3, s::BOOL, 4, 0, op == 30 ? 53 : 52, 5, 0, 0, op, 0},
         {{99, 0, 0}},
         s::fault(s::Status::Invalid)});
  }
  const std::vector<std::vector<int32_t>> malformed = {{},
                                                       {s::MAGIC},
                                                       {0, s::INT4, 0, 1, 1},
                                                       {s::MAGIC, 99, 0, 1, 1},
                                                       {s::MAGIC, s::INT4, -1},
                                                       {s::MAGIC, s::INT4, 0},
                                                       {s::MAGIC, s::INT4, 0, 1},
                                                       {s::MAGIC, s::INT4, 0, 10, 0},
                                                       {s::MAGIC, s::INT4, 0, 4, 0},
                                                       {s::MAGIC, s::INT4, 0, 5, 0},
                                                       {s::MAGIC, s::INT4, 0, 6, 42},
                                                       {s::MAGIC, s::BOOL, 0, 2, 1, 6, 42},
                                                       {s::MAGIC, s::INT4, 0, 55, 1},
                                                       {s::MAGIC, s::INT4, 0, 1, 1, 55, 2},
                                                       {s::MAGIC, s::INT4, 0, 1, 1, 2, 1, 55, 3},
                                                       {s::MAGIC, s::INT4, 0, 1, 1, 1, 1, 55, -1},
                                                       {s::MAGIC, s::INT4, 0, 1, 1, 1, 1, 55, 2},
                                                       {s::MAGIC, s::INT4, 0, 1, 1, 1, 1, 55, 99},
                                                       {s::MAGIC, s::INT4, 0, 1, 1, 5, 1},
                                                       {s::MAGIC, s::INT4, 0, 1, 1, 5, 0},
                                                       {s::MAGIC, s::BOOL, 0, 2, 2},
                                                       {s::MAGIC, s::INT4, 0, 3, 99},
                                                       {s::MAGIC, s::INT4, 0, 0, 0},
                                                       {s::MAGIC, s::INT4, 0, 0, -1},
                                                       {s::MAGIC, s::INT4, 0, 50, 0},
                                                       {s::MAGIC, s::INT4, 0, 50, -1},
                                                       {s::MAGIC, s::INT4, 0, 50, 2},
                                                       {s::MAGIC, s::INT4, 0, 54, 1},
                                                       {s::MAGIC, s::INT4, 0, 51, 1},
                                                       {s::MAGIC, s::INT4, 0, 1, 1, 51, 2},
                                                       {s::MAGIC, s::INT4, 0, 1, 1, 15, 1},
                                                       {s::MAGIC, s::INT4, 0, 1, 1, 999, 0},
                                                       {s::MAGIC, s::INT4, 0, 1, 1, 32, 0},
                                                       {s::MAGIC, s::BOOL, 0, 2, 1, 15, 0},
                                                       {s::MAGIC, s::INT4, 0, 1, 1, 2, 1, 10, 0},
                                                       {s::MAGIC, s::INT4, 0, 1, 1, 1, 2, 30, 0},
                                                       {s::MAGIC, s::INT4, 0, 1, 1, 4, 1},
                                                       {s::MAGIC, s::INT4, 0, 2, 1},
                                                       {s::MAGIC, s::INT4, 0, 1, 1, 1, 2}};
  for (const auto& code : malformed)
    cases.push_back({"malformed executed contract", code, {}, s::fault(s::Status::Invalid)});
  for (s::Value value :
       {s::Value{99, 0, 0}, s::Value{s::INT4, 0, 2}, s::Value{s::INT4, 7, 1}, boolean(true)})
    cases.push_back({"invalid input binding",
                     {s::MAGIC, s::INT4, 1, s::INT4, 0, 0},
                     {value},
                     s::fault(s::Status::Invalid)});
  cases.push_back({"noncanonical Boolean",
                   {s::MAGIC, s::BOOL, 1, s::BOOL, 0, 0},
                   {{s::BOOL, 7, 0}},
                   s::fault(s::Status::Invalid)});
  cases.push_back({"binding count mismatch",
                   {s::MAGIC, s::INT4, 0, 1, 1},
                   {integer(1)},
                   s::fault(s::Status::Invalid)});
}

bool equal(s::Result a, s::Result b) {
  return a.status == b.status && a.value.type == b.value.type &&
         a.value.is_null == b.value.is_null && a.value.payload == b.value.payload;
}

template <class T>
struct DeviceBuffer {
  T* data;
  explicit DeviceBuffer(size_t count) : data(sycl::malloc_device<T>(count, *g_queue)) {
    if (!data)
      throw std::bad_alloc();
  }
  DeviceBuffer(const DeviceBuffer&) = delete;
  DeviceBuffer& operator=(const DeviceBuffer&) = delete;
  ~DeviceBuffer() { sycl::free(data, *g_queue); }
};

void run() {
  auto cases = corpus();
  for (const auto& test : cases) {
    s::Proof proof;
    if (!s::validate_program(test.words.data(), test.words.size(), &proof) ||
        proof.input_count != test.inputs.size() || proof.result_type != test.words[1] ||
        proof.instruction_count != (test.words.size() - 3 - proof.input_count) / 2 ||
        proof.max_stack == 0 || proof.max_stack > s::MAX_STACK)
      throw std::runtime_error("full program proof failed: " + test.name);
  }
  const std::vector<std::vector<int32_t>> invalid_programs = {
      {s::MAGIC, s::BOOL, 0, 26, 0},
      {s::MAGIC, s::BOOL, 0, 1, 1, 26, 0},
      {s::MAGIC, s::BOOL, 0, 1, 1, 2, 1, 26, 0},
      {s::MAGIC, s::BOOL, 0, 1, 1, 1, 1, 26, 1},
      {s::MAGIC, s::INT4, 0, 1, 1, 1, 1, 26, 0},
      {s::MAGIC, s::DATE, 0, 7, s::DATE_MIN - 1},
      {s::MAGIC, s::DATE, 0, 7, s::DATE_END},
      {s::MAGIC, s::DATE, 0, 7, INT32_MIN + 1},
      {s::MAGIC, s::DATE, 0, 7, INT32_MAX - 1},
      {s::MAGIC, s::DATE, 0, 70, 0},
      {s::MAGIC, s::DATE, 0, 7, 0, 1, 1, 70, 1},
      {s::MAGIC, s::DATE, 0, 1, 0, 1, 1, 70, 0},
      {s::MAGIC, s::DATE, 0, 7, 0, 7, 1, 71, 0},
      {s::MAGIC, s::DATE, 0, 7, 0, 1, 1, 72, 0},
      {s::MAGIC, s::INT4, 0, 7, 0, 1, 1, 73, 0},
      {s::MAGIC, s::INT4, 0, 7, 0, 7, 1, 11, 0},
      {s::MAGIC, s::DATE, 0, 7, 0, 60, 1},
      // NULLIF/extrema require two equal-typed operands and a matching result.
      {s::MAGIC, s::INT4, 0, 27, 0},
      {s::MAGIC, s::INT4, 0, 1, 1, 27, 0},
      {s::MAGIC, s::INT4, 0, 1, 1, 2, 1, 27, 0},
      {s::MAGIC, s::INT4, 0, 1, 1, 1, 2, 27, 1},
      {s::MAGIC, s::BOOL, 0, 1, 1, 1, 2, 27, 0},
      {s::MAGIC, s::INT4, 0, 28, 0},
      {s::MAGIC, s::INT4, 0, 1, 1, 28, 0},
      {s::MAGIC, s::INT4, 0, 1, 1, 2, 1, 28, 0},
      {s::MAGIC, s::INT4, 0, 1, 1, 1, 2, 28, 1},
      {s::MAGIC, s::BOOL, 0, 1, 1, 1, 2, 28, 0},
      {s::MAGIC, s::INT4, 0, 29, 0},
      {s::MAGIC, s::INT4, 0, 1, 1, 29, 0},
      {s::MAGIC, s::INT4, 0, 1, 1, 2, 1, 29, 0},
      {s::MAGIC, s::INT4, 0, 1, 1, 1, 2, 29, 1},
      {s::MAGIC, s::BOOL, 0, 1, 1, 1, 2, 29, 0},
      // Invalid inactive CASE arm, incompatible merge type/depth, unreachable
      // instruction, unused invalid input, backward jump and noncanonical arg.
      {s::MAGIC, s::INT4, 0, 2, 0, 51, 4, 99, 0, 50, 5, 1, 42},
      {s::MAGIC, s::INT4, 0, 2, 0, 51, 4, 2, 1, 50, 5, 1, 42},
      {s::MAGIC, s::INT4, 0, 2, 0, 51, 3, 1, 1, 1, 42},
      {s::MAGIC, s::INT4, 0, 50, 2, 1, 1, 1, 42},
      {s::MAGIC, s::INT4, 1, 99, 1, 42},
      {s::MAGIC, s::INT4, 0, 1, 42, 50, 0},
      {s::MAGIC, s::INT4, 0, 1, 42, 15, 1},
      {s::MAGIC, s::INT4, 0, 5, 0, 1, 42},
      {s::MAGIC, s::INT4, 0, 1, 42, 5, 1, 1, 42},
      {s::MAGIC, s::INT4, 0, 1, 42, 5, 0},
      {s::MAGIC, s::INT4, 0, 6, 42},
      {s::MAGIC, s::BOOL, 0, 2, 1, 6, 42},
      {s::MAGIC, s::INT4, 0, 55, 1},
      {s::MAGIC, s::INT4, 0, 1, 1, 55, 2},
      {s::MAGIC, s::INT4, 0, 1, 1, 2, 1, 55, 3},
      {s::MAGIC, s::INT4, 0, 1, 1, 1, 1, 55, -1},
      {s::MAGIC, s::INT4, 0, 1, 1, 1, 1, 55, 2},
      {s::MAGIC, s::INT4, 0, 1, 1, 1, 1, 55, 99},
  };
  for (const auto& words : invalid_programs) {
    s::Proof proof{99, 99, 99, 99};
    if (s::validate_program(words.data(), words.size(), &proof) || proof.input_count ||
        proof.instruction_count || proof.max_stack || proof.result_type)
      throw std::runtime_error("invalid full CFG accepted or proof published");
  }
  for (int32_t op = 60; op <= 69; ++op) {
    for (const auto& body : std::vector<std::vector<int32_t>>{
             {op, 42}, {2, 0, op, 42},
             op < 65 ? std::vector<int32_t>{3, s::BOOL, op, 42}
                     : std::vector<int32_t>{1, 1, op, 42},
             {2, 0, 1, 1, op, 42}}) {
      std::vector<int32_t> words{s::MAGIC, s::INT4, 0};
      words.insert(words.end(), body.begin(), body.end());
      if (s::validate_program(words.data(), words.size(), nullptr))
        throw std::runtime_error("invalid fused arithmetic accepted");
      cases.push_back({"invalid fused arithmetic", words, {}, s::fault(s::Status::Invalid)});
    }
  }
  std::printf("Full CFG proof: 468 shared valid, %zu invalid programs passed\n",
              invalid_programs.size() + 40);
  boundary_cases(cases);
  const std::vector<int32_t> mask_first{s::MAGIC, s::BOOL, 1, s::INT4,
      8, 0, 51, 8, 0, 0, 1, 0, 13, 0, 1, 0, 24, 0, 50, 9, 2, 0};
  const std::vector<int32_t> scalar_first{s::MAGIC, s::BOOL, 1, s::INT4,
      0, 0, 1, 0, 13, 0, 1, 0, 24, 0, 51, 8, 8, 0, 50, 9, 2, 0};
  for (auto mask : {boolean(false), null(s::BOOL), boolean(true)}) {
    cases.push_back({"mask before throwing scalar", mask_first, {integer(1)},
        mask.payload == 1 ? s::fault(s::Status::DivisionByZero) : ok(boolean(false)), mask});
    cases.push_back({"scalar before mask", scalar_first, {integer(1)},
        s::fault(s::Status::DivisionByZero), mask});
    cases.push_back({"exact SQL predicate mask", {s::MAGIC, s::BOOL, 0, 8, 0}, {}, ok(mask), mask});
  }
  for (auto mask : {s::Value{0, 0, 0}, integer(1), s::Value{s::BOOL, 2, 0}})
    cases.push_back({"invalid predicate mask context", {s::MAGIC, s::BOOL, 0, 8, 0}, {},
        s::fault(s::Status::Invalid), mask});
  s::Proof mask_proof;
  if (!s::validate_program(mask_first.data(), mask_first.size(), &mask_proof) ||
      !mask_proof.uses_predicate_mask)
    throw std::runtime_error("missing predicate-mask context proof");
  const int32_t invalid_mask_operand[] = {s::MAGIC, s::BOOL, 0, 8, 1};
  if (s::validate_program(invalid_mask_operand, 5, nullptr))
    throw std::runtime_error("noncanonical mask operand accepted");
  // Invalid resident DATE payloads are rejected at an executed LOAD, even
  // though their wire type and input pointer are otherwise well formed.
  for (int32_t value : {INT32_MIN + 1, s::DATE_MIN - 1, s::DATE_END, INT32_MAX - 1}) {
    cases.push_back({"invalid DATE input", {s::MAGIC, s::DATE, 1, s::DATE, 0, 0},
                     {{s::DATE, value, 0}}, s::fault(s::Status::Invalid)});
  }
  std::vector<int32_t> words;
  std::vector<s::Value> inputs;
  std::vector<Binding> bindings;
  for (const auto& test : cases) {
    auto host = s::evaluate(test.words.data(), test.words.size(), test.inputs.size(),
                            [&](size_t index) { return test.inputs.at(index); }, test.mask);
    if (!equal(host, test.expected))
      throw std::runtime_error("host mismatch: " + test.name);
    bindings.push_back({words.size(), test.words.size(), inputs.size(), test.inputs.size(), test.mask});
    words.insert(words.end(), test.words.begin(), test.words.end());
    inputs.insert(inputs.end(), test.inputs.begin(), test.inputs.end());
  }
  DeviceBuffer<int32_t> device_words(words.size());
  DeviceBuffer<s::Value> device_inputs(inputs.size());
  DeviceBuffer<Binding> device_bindings(bindings.size());
  DeviceBuffer<s::Result> device_results(cases.size());
  g_queue->memcpy(device_words.data, words.data(), words.size() * sizeof(int32_t)).wait_and_throw();
  g_queue->memcpy(device_inputs.data, inputs.data(), inputs.size() * sizeof(s::Value))
      .wait_and_throw();
  g_queue->memcpy(device_bindings.data, bindings.data(), bindings.size() * sizeof(Binding))
      .wait_and_throw();
  // Poison all outputs; a missing dispatch cannot accidentally pass the oracle.
  g_queue->memset(device_results.data, 0xa5, cases.size() * sizeof(s::Result)).wait_and_throw();
  const auto* wire = device_words.data;
  const auto* values = device_inputs.data;
  const auto* rows = device_bindings.data;
  auto* results = device_results.data;
  g_queue
      ->parallel_for(sycl::range<1>(cases.size()),
                     [=](sycl::id<1> id) {
                       const auto binding = rows[id[0]];
                       results[id[0]] = s::evaluate(
                           wire + binding.word_offset, binding.word_count, binding.input_count,
                           [=](size_t index) { return values[binding.input_offset + index]; }, binding.mask);
                     })
      .wait_and_throw();
  std::vector<s::Result> output(cases.size());
  g_queue->memcpy(output.data(), results, output.size() * sizeof(s::Result)).wait_and_throw();
  for (size_t i = 0; i < cases.size(); ++i) {
    if (!equal(output[i], cases[i].expected)) {
      std::fprintf(stderr, "case %zu %s: actual=(%d,%d,%d,%d) expected=(%d,%d,%d,%d)\n", i,
                   cases[i].name.c_str(), int(output[i].status), output[i].value.type,
                   output[i].value.payload, output[i].value.is_null, int(cases[i].expected.status),
                   cases[i].expected.value.type, cases[i].expected.value.payload,
                   cases[i].expected.value.is_null);
      throw std::runtime_error("device scalar mismatch");
    }
  }
  std::printf(
      "SPG1: %zu host/device cases passed (468 shared Rust oracle cases); no SQL admission\n",
      cases.size());
}
}  // namespace

int main() {
  if (pgaccel_init() != PGACCEL_OK || !g_queue) {
    std::fprintf(stderr, "scalar test requires a real GPU\n");
    return 1;
  }
  int status = 0;
  try {
    run();
  } catch (const std::exception& error) {
    std::fprintf(stderr, "%s\n", error.what());
    status = 1;
  }
  pgaccel_shutdown();
  return status;
}
