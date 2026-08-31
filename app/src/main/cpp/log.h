#pragma once

#ifdef __ANDROID__
#include <android/log.h>
#define LAU_LOGI(...) __android_log_print(ANDROID_LOG_INFO,  "lanmic", __VA_ARGS__)
#define LAU_LOGW(...) __android_log_print(ANDROID_LOG_WARN,  "lanmic", __VA_ARGS__)
#define LAU_LOGE(...) __android_log_print(ANDROID_LOG_ERROR, "lanmic", __VA_ARGS__)
#else
#include <cstdio>
#define LAU_LOGI(...) do { fprintf(stderr, "[I] "); fprintf(stderr, __VA_ARGS__); fputc('\n', stderr); } while (0)
#define LAU_LOGW(...) do { fprintf(stderr, "[W] "); fprintf(stderr, __VA_ARGS__); fputc('\n', stderr); } while (0)
#define LAU_LOGE(...) do { fprintf(stderr, "[E] "); fprintf(stderr, __VA_ARGS__); fputc('\n', stderr); } while (0)
#endif
