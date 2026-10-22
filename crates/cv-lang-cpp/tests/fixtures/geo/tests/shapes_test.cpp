#include <gtest/gtest.h>

#include "geo/circle.hpp"
#include "geo/rect.hpp"

TEST(Shapes, CircleArea) {
  geo::Circle c(1.0);
  EXPECT_NEAR(c.area(), 3.14159, 1e-3);
}

TEST(Shapes, RectArea) {
  geo::Rect r(2.0, 3.0);
  EXPECT_EQ(r.area(), 6.0);
}
