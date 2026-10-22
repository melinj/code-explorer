#pragma once
#include "geo/shape.hpp"

namespace geo {

class Circle : public Shape {
 public:
  explicit Circle(double r) : r_(r) {}
  double area() const override;
  double radius() const { return r_; }

 private:
  double r_;
};

}  // namespace geo
