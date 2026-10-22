#include <stdio.h>
#include "../include/geo/version.h"

struct point {
  int x, y;
};

static int dot(struct point a, struct point b) { return a.x * b.x + a.y * b.y; }

int main(void) {
  struct point p = {1, 2};
  printf("%s %d\n", GEO_VERSION, dot(p, p));
  return 0;
}
