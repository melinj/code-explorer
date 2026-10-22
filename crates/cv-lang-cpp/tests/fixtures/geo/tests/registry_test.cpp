#include "gtest/gtest.h"
#include "geo/registry.hpp"

TEST(Registry, PutGet) {
  geo::reg::Registry r;
  r.put("a", 1);
  EXPECT_EQ(r.get("geo/item/a"), 1);
}
