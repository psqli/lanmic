cmake_minimum_required(VERSION 3.22.1)
project(lanmic LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(oboe REQUIRED CONFIG)

add_library(lanmic SHARED
        jni_bridge.cpp
        transmitter.cpp
        receiver.cpp
        udp_socket.cpp)

target_compile_options(lanmic PRIVATE
        -Wall -Wextra -Wno-unused-parameter
        -ffast-math -fno-finite-math-only)

target_link_libraries(lanmic
        oboe::oboe
        android
        log)
