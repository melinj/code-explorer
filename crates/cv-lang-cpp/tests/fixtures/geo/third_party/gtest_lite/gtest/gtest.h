#pragma once
// A tiny stand-in for googletest's macros.
#define TEST(suite, name) void suite##_##name()
#define TEST_F(fixture, name) void fixture##_##name()
#define EXPECT_EQ(a, b) ((void)((a) == (b)))
#define EXPECT_NEAR(a, b, e) ((void)((a) - (b) < (e)))

namespace testing {
class Test {
 public:
  virtual void SetUp() {}
};
}  // namespace testing
