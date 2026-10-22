#include "geo/circle.hpp"
#include "geo/rect.hpp"
#include "detail/math.h"

namespace geo {

std::string Shape::name() const { return "shape"; }

double Circle::area() const {
  return detail::pi() * detail::square(r_);
}

Rect::Rect(double w, double h) : Size{w, h} {}

double Rect::area() const {
  return w * h;
}

}  // namespace geo
