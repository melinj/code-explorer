#pragma once
#include <map>
#include <string>

namespace geo::reg {

constexpr const char* kPrefix = "geo/";

class Registry {
 public:
  void put(const std::string& key, int value);
  int get(const std::string& key) const;

 private:
  std::map<std::string, int> items_;
};

}  // namespace geo::reg
