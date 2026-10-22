#pragma once
#include <string>

namespace geo {

class Shape {
 public:
  virtual ~Shape() = default;
  virtual double area() const = 0;
  std::string name() const;

 protected:
  int id_ = 0;
};

}  // namespace geo
