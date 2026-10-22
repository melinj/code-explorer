#include <geo/canvas.hpp>
#include <tinyfmt/format.hpp>
#include "clock.h"

namespace geo {

void Canvas::add(std::unique_ptr<Shape> s) {
  shapes_.push_back(std::move(s));
  this->log("added");
}

double Canvas::total_area() const {
  double sum = 0;
  for (const auto& s : shapes_) sum += s->area();
  return clamp(sum, 0.0, 1e9);
}

void Canvas::draw() {
  auto t = detail::now_ms();
  log(tinyfmt::format("draw {} shapes", shapes_.size()).c_str());
  (void)t;
}

void Canvas::log(const char* msg) {
  tinyfmt::print(msg);
}

}  // namespace geo
