#include <catch2/catch.hpp>

#include "geo/util.hpp"

TEST_CASE("units convert to millimetres") {
  REQUIRE(geo::util::to_mm(1.0, geo::util::Unit::Cm) == 10.0);
}

TEST_CASE("bits count") {
  geo::util::Bits b{};
  b.u = 0b1011;
  REQUIRE(geo::util::popcount(b) == 3);
}
