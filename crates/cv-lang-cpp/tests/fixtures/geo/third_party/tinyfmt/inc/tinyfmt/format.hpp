#pragma once
#include <cstdio>
#include <string>

#include "detail.hpp"

namespace tinyfmt {

template <typename... Args>
std::string format(const char* fmt, Args&&... args) {
  return detail::render(fmt, sizeof...(args));
}

inline void print(const char* s) { std::fputs(s, stdout); }

}  // namespace tinyfmt
