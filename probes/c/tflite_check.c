/* tflite_check <libtflite path> <model.tflite> [invokes [resolver tag]]: loads the model the way
 * io.sdk does (Yandex resolver API; the stock TfLiteInterpreterCreate has no op resolver in this
 * build), lists the tensors and times invokes on zero input. */
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

typedef struct {
    float scale;
    int32_t zero_point;
} QParams;

#define SYM(type, name)                                                  \
    type name = (type)dlsym(lib, "TfLite" #name);                         \
    if (!name) {                                                          \
        fprintf(stderr, "no TfLite%s\n", #name);                          \
        return 1;                                                         \
    }

typedef void *(*ModelCreateFromFile_t)(const char *);
typedef void *(*InterpreterOptionsCreate_t)(void);
typedef void (*InterpreterOptionsSetNumThreads_t)(void *, int32_t);
typedef void (*AddCustomOpsRegistrators_t)(void *);
typedef void *(*CustomOpResolverCreate_t)(const char *);
typedef void *(*InterpreterCreateWithCustomResolver_t)(const void *, void *, const void *);
typedef void (*InterpreterOptionsDelete_t)(void *);
typedef int (*InterpreterAllocateTensors_t)(void *);
typedef int (*InterpreterInvoke_t)(void *);
typedef int32_t (*InterpreterGetInputTensorCount_t)(const void *);
typedef int32_t (*InterpreterGetOutputTensorCount_t)(const void *);
typedef void *(*InterpreterGetInputTensor_t)(const void *, int32_t);
typedef const void *(*InterpreterGetOutputTensor_t)(const void *, int32_t);
typedef const char *(*TensorName_t)(const void *);
typedef int (*TensorType_t)(const void *);
typedef size_t (*TensorByteSize_t)(const void *);
typedef QParams (*TensorQuantizationParams_t)(const void *);

static double now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1e3 + ts.tv_nsec / 1e6;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s <libtflite> <model.tflite> [invokes [resolver tag]]\n", argv[0]);
        return 2;
    }
    setvbuf(stdout, NULL, _IONBF, 0);
    int invokes = argc > 3 ? atoi(argv[3]) : 200;
    double t0 = now_ms();
    void *lib = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!lib) {
        fprintf(stderr, "dlopen: %s\n", dlerror());
        return 1;
    }
    SYM(ModelCreateFromFile_t, ModelCreateFromFile)
    SYM(InterpreterOptionsCreate_t, InterpreterOptionsCreate)
    SYM(InterpreterOptionsSetNumThreads_t, InterpreterOptionsSetNumThreads)
    SYM(AddCustomOpsRegistrators_t, AddCustomOpsRegistrators)
    SYM(CustomOpResolverCreate_t, CustomOpResolverCreate)
    SYM(InterpreterCreateWithCustomResolver_t, InterpreterCreateWithCustomResolver)
    SYM(InterpreterOptionsDelete_t, InterpreterOptionsDelete)
    SYM(InterpreterAllocateTensors_t, InterpreterAllocateTensors)
    SYM(InterpreterInvoke_t, InterpreterInvoke)
    SYM(InterpreterGetInputTensorCount_t, InterpreterGetInputTensorCount)
    SYM(InterpreterGetOutputTensorCount_t, InterpreterGetOutputTensorCount)
    SYM(InterpreterGetInputTensor_t, InterpreterGetInputTensor)
    SYM(InterpreterGetOutputTensor_t, InterpreterGetOutputTensor)
    SYM(TensorName_t, TensorName)
    SYM(TensorType_t, TensorType)
    SYM(TensorByteSize_t, TensorByteSize)
    SYM(TensorQuantizationParams_t, TensorQuantizationParams)

    printf("dlopen ok %.1f ms\n", now_ms() - t0);
    void *model = ModelCreateFromFile(argv[2]);
    if (!model) {
        fprintf(stderr, "cannot load %s\n", argv[2]);
        return 1;
    }
    printf("model ok\n");
    const char *tag = argc > 4 ? argv[4] : "SPOTTER_ACTIVATION";
    void *opts = InterpreterOptionsCreate();
    InterpreterOptionsSetNumThreads(opts, 1);
    AddCustomOpsRegistrators(opts);
    void *resolver = CustomOpResolverCreate(tag);
    if (!resolver) {
        fprintf(stderr, "no resolver '%s'\n", tag);
        return 1;
    }
    void *interp = InterpreterCreateWithCustomResolver(model, resolver, opts);
    InterpreterOptionsDelete(opts);
    if (!interp || InterpreterAllocateTensors(interp) != 0) {
        fprintf(stderr, "interpreter setup failed\n");
        return 1;
    }
    printf("load %.1f ms\n", now_ms() - t0);
    for (int32_t i = 0; i < InterpreterGetInputTensorCount(interp); i++) {
        void *t = InterpreterGetInputTensor(interp, i);
        QParams q = TensorQuantizationParams(t);
        printf("in  %-10s type %d bytes %zu zp %d\n", TensorName(t), TensorType(t),
               TensorByteSize(t), q.zero_point);
    }
    for (int32_t i = 0; i < InterpreterGetOutputTensorCount(interp); i++) {
        const void *t = InterpreterGetOutputTensor(interp, i);
        printf("out %-10s type %d bytes %zu\n", TensorName(t), TensorType(t), TensorByteSize(t));
    }
    double best = 1e9, total = 0;
    for (int i = 0; i < invokes; i++) {
        double s = now_ms();
        if (InterpreterInvoke(interp) != 0) {
            fprintf(stderr, "invoke failed\n");
            return 1;
        }
        double d = now_ms() - s;
        total += d;
        if (d < best) {
            best = d;
        }
    }
    printf("invoke mean %.2f ms, best %.2f ms over %d (one call = 60 ms of audio)\n",
           total / invokes, best, invokes);
    return 0;
}
