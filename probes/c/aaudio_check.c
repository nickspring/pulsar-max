/* aaudio_check [seconds] [low]: plays silence through AAudio (usage MEDIA, 48 kHz s16 stereo) and
 * prints the stream setup and, twice a second, the presentation timestamp and the output latency it
 * implies. "low" asks for AAUDIO_PERFORMANCE_MODE_LOW_LATENCY. Silent: no volume consent needed. */
#include <aaudio/AAudio.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define RATE 48000
#define CHANNELS 2
#define CHUNK (RATE / 100)

static int64_t now_ns(clockid_t clock) {
    struct timespec ts;
    clock_gettime(clock, &ts);
    return (int64_t)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static int check(aaudio_result_t r, const char *what) {
    if (r < 0) {
        fprintf(stderr, "%s: %s\n", what, AAudio_convertResultToText(r));
        return 0;
    }
    return 1;
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);
    int seconds = argc > 1 ? atoi(argv[1]) : 5;
    int low = argc > 2 && strcmp(argv[2], "low") == 0;
    AAudioStreamBuilder *b;
    if (!check(AAudio_createStreamBuilder(&b), "createStreamBuilder")) {
        return 1;
    }
    AAudioStreamBuilder_setFormat(b, AAUDIO_FORMAT_PCM_I16);
    AAudioStreamBuilder_setChannelCount(b, CHANNELS);
    AAudioStreamBuilder_setSampleRate(b, RATE);
    AAudioStreamBuilder_setUsage(b, AAUDIO_USAGE_MEDIA);
    AAudioStreamBuilder_setContentType(b, AAUDIO_CONTENT_TYPE_MUSIC);
    AAudioStreamBuilder_setPerformanceMode(
        b, low ? AAUDIO_PERFORMANCE_MODE_LOW_LATENCY : AAUDIO_PERFORMANCE_MODE_NONE);
    AAudioStream *s;
    int64_t t0 = now_ns(CLOCK_MONOTONIC);
    if (!check(AAudioStreamBuilder_openStream(b, &s), "openStream")) {
        return 1;
    }
    printf("open %.1f ms: rate %d ch %d fmt %d perf %d sharing %d burst %d buffer %d capacity %d\n",
           (now_ns(CLOCK_MONOTONIC) - t0) / 1e6, AAudioStream_getSampleRate(s),
           AAudioStream_getChannelCount(s), AAudioStream_getFormat(s),
           AAudioStream_getPerformanceMode(s), AAudioStream_getSharingMode(s),
           AAudioStream_getFramesPerBurst(s), AAudioStream_getBufferSizeInFrames(s),
           AAudioStream_getBufferCapacityInFrames(s));
    if (!check(AAudioStream_requestStart(s), "requestStart")) {
        return 1;
    }
    static int16_t silence[CHUNK * CHANNELS];
    int64_t start = now_ns(CLOCK_MONOTONIC), next_report = start;
    int64_t first_ts = -1;
    while (now_ns(CLOCK_MONOTONIC) - start < (int64_t)seconds * 1000000000LL) {
        aaudio_result_t n = AAudioStream_write(s, silence, CHUNK, 20 * 1000000LL);
        if (!check(n < 0 ? n : 0, "write")) {
            break;
        }
        int64_t now = now_ns(CLOCK_MONOTONIC);
        if (now < next_report) {
            continue;
        }
        next_report = now + 500000000LL;
        int64_t pos = 0, ts = 0;
        aaudio_result_t r = AAudioStream_getTimestamp(s, CLOCK_MONOTONIC, &pos, &ts);
        int64_t written = AAudioStream_getFramesWritten(s), read = AAudioStream_getFramesRead(s);
        if (r < 0) {
            printf("%6.0f ms: no timestamp (%s) written %lld read %lld\n", (now - start) / 1e6,
                   AAudio_convertResultToText(r), (long long)written, (long long)read);
            continue;
        }
        if (first_ts < 0) {
            first_ts = now - start;
        }
        double latency_ms = (written - pos) * 1000.0 / RATE - (now - ts) / 1e6;
        printf("%6.0f ms: pos %lld at -%.2f ms, written %lld read %lld, latency %.2f ms, xruns %d\n",
               (now - start) / 1e6, (long long)pos, (now - ts) / 1e6, (long long)written,
               (long long)read, latency_ms, AAudioStream_getXRunCount(s));
    }
    printf("first timestamp after %lld ms\n", (long long)(first_ts / 1000000LL));
    AAudioStream_requestStop(s);
    AAudioStream_close(s);
    return 0;
}
