add_library(geo shapes.cpp canvas.cpp registry.cpp util.cc detail/math.cpp)
target_include_directories(geo
  PUBLIC $<BUILD_INTERFACE:${PROJECT_SOURCE_DIR}/include>
  PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/detail "${CMAKE_SOURCE_DIR}/third_party/tinyfmt/inc"
)
