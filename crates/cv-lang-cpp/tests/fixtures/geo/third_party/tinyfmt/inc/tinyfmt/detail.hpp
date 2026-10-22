#pragma once
#include <string>

namespace tinyfmt::detail {

inline std::string render(const char* fmt, unsigned long n) {
  return std::string(fmt) + "#" + std::to_string(n);
}

}  // namespace tinyfmt::detail
