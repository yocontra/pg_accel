#include <cstdio>
int calls = 0;
int bound() { ++calls; return 1; }
int consume(const int values[bound()]) { return values[0]; }
int main() {
  const int value = 7;
  const int result = consume(&value);
  std::printf("parameter-bound calls=%d, result=%d\n", calls, result);
  return 0;
}
