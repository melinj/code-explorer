add_executable(geo_tests shapes_test.cpp canvas_test.cpp registry_test.cpp)
target_include_directories(geo_tests SYSTEM PRIVATE ../third_party/gtest_lite)
target_link_libraries(geo_tests geo)
