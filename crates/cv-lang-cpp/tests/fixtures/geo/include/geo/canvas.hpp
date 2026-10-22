#pragma once
#include <memory>
#include <vector>

#include "geo/shape.hpp"

namespace geo {

template <typename T>
T clamp(T v, T lo, T hi) {
  return v < lo ? lo : (v > hi ? hi : v);
}

class Canvas {
 public:
  void add(std::unique_ptr<Shape> s);
  double total_area() const;
  void draw();

 private:
  void log(const char* msg);
  std::vector<std::unique_ptr<Shape>> shapes_;
};

}  // namespace geo
