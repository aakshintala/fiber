/* Peak-RSS sampler: reads VmHWM from /proc/self/status on tick. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define LINE_MAX 256
#define SAMPLES_MAX 1024

typedef struct {
    long times_ms[SAMPLES_MAX];
    long rss_kb[SAMPLES_MAX];
    size_t count;
} sampler_t;

static long read_vmhwm_kb(void) {
    FILE *f = fopen("/proc/self/status", "r");
    if (!f) {
        return -1;
    }
    char line[LINE_MAX];
    long value = -1;
    while (fgets(line, sizeof line, f)) {
        if (strncmp(line, "VmHWM:", 6) == 0) {
            value = atol(line + 6);
            break;
        }
    }
    fclose(f);
    return value;
}

static int cmp_long(const void *a, const void *b) {
    long x = *(const long *)a, y = *(const long *)b;
    return (x > y) - (x < y);
}

long sampler_median(sampler_t *s) {
    if (s->count == 0) {
        return -1;
    }
    qsort(s->rss_kb, s->count, sizeof(long), cmp_long);
    return s->rss_kb[s->count / 2];
}
