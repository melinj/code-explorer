#include "geo/registry.hpp"

#include <string>

namespace geo::reg {

namespace {
const std::string kTopic = std::string(kPrefix) + "items";
constexpr const char* kSep = "/";
}  // namespace

void Registry::put(const std::string& key, int value) {
  items_[std::string("geo/item") + kSep + key] = value;
}

int Registry::get(const std::string& key) const {
  auto it = items_.find(key);
  return it == items_.end() ? 0 : it->second;
}

}  // namespace geo::reg
