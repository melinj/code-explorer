#include <geo/canvas.hpp>
#include <geo/circle.hpp>
#include <iostream>

#include "geo/version.h"

int main() {
  geo::Canvas c;
  c.add(std::make_unique<geo::Circle>(2.0));
  c.draw();
  std::cout << GEO_VERSION << " " << c.total_area() << "\n";
  return 0;
}
