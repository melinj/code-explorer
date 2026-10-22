#pragma once
#include "shape.hpp"

namespace geo {

struct Size {
  double w;
  double h;
};

class Rect : public Shape, private Size {
 public:
  Rect(double w, double h);
  double area() const override;
};

}  // namespace geo
