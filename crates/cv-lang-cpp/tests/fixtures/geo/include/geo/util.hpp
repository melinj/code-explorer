#pragma once
#include <cstdint>

namespace geo {
namespace util {

enum class Unit { Mm, Cm, In };

union Bits {
  std::uint32_t u;
  float f;
};

template <typename T, int N>
struct Fixed {
  T data[N];
  int size() const { return N; }
};

double to_mm(double v, Unit u);
int popcount(Bits b);

}  // namespace util
}  // namespace geo
