#include "geo/util.hpp"

namespace geo::util {

namespace {
double factor(Unit u) {
  switch (u) {
    case Unit::Mm: return 1.0;
    case Unit::Cm: return 10.0;
    case Unit::In: return 25.4;
  }
  return 1.0;
}
}  // namespace

double to_mm(double v, Unit u) { return v * factor(u); }

int popcount(Bits b) {
  int n = 0;
  for (auto x = b.u; x; x &= x - 1) ++n;
  return n;
}

}  // namespace geo::util
