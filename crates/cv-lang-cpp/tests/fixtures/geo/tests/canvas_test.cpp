#include <gtest/gtest.h>
#include <geo/canvas.hpp>
#include <geo/circle.hpp>

class CanvasTest : public testing::Test {
 protected:
  void SetUp() override { canvas.add(std::make_unique<geo::Circle>(1.0)); }
  geo::Canvas canvas;
};

TEST_F(CanvasTest, TotalArea) {
  EXPECT_NEAR(canvas.total_area(), 3.14159, 1e-3);
}
