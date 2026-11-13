# Builds sendspin-cpp and its dependencies as static libraries (copy of pulsar's
# satellite/third_party/CMakeLists.txt). Sources come from src/ (populated by fetch.sh); nothing
# is downloaded here. Driven by the top-level Makefile with the NDK's CMake toolchain.
cmake_minimum_required(VERSION 3.18)
project(cucumber_sendspin_deps C CXX)

set(_src ${CMAKE_CURRENT_SOURCE_DIR}/src)
set(FETCHCONTENT_FULLY_DISCONNECTED ON CACHE BOOL "" FORCE)
set(FETCHCONTENT_SOURCE_DIR_ARDUINOJSON ${_src}/ArduinoJson CACHE PATH "" FORCE)
set(FETCHCONTENT_SOURCE_DIR_MICRO_FLAC ${_src}/micro-flac CACHE PATH "" FORCE)
set(FETCHCONTENT_SOURCE_DIR_MICRO_OPUS ${_src}/micro-opus CACHE PATH "" FORCE)
set(FETCHCONTENT_SOURCE_DIR_IXWEBSOCKET ${_src}/IXWebSocket CACHE PATH "" FORCE)
set(FETCHCONTENT_SOURCE_DIR_NOISE_C ${_src}/noise-c CACHE PATH "" FORCE)

# Roles: player (with Opus), metadata, controller (transport commands) and visualizer (LED music
# visualization). Keep in sync with ROLES in third_party/pulsar-sendspin/build.rs.
set(SENDSPIN_ENABLE_PLAYER ON CACHE BOOL "" FORCE)
set(SENDSPIN_ENABLE_OPUS ON CACHE BOOL "" FORCE)
set(SENDSPIN_ENABLE_METADATA ON CACHE BOOL "" FORCE)
set(SENDSPIN_ENABLE_CONTROLLER ON CACHE BOOL "" FORCE)
set(SENDSPIN_ENABLE_COLOR OFF CACHE BOOL "" FORCE)
set(SENDSPIN_ENABLE_ARTWORK OFF CACHE BOOL "" FORCE)
set(SENDSPIN_ENABLE_VISUALIZER ON CACHE BOOL "" FORCE)
set(SENDSPIN_ENABLE_SOURCE OFF CACHE BOOL "" FORCE)
set(BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
set(SENDSPIN_USE_CCACHE OFF CACHE BOOL "" FORCE)
set(MICRO_FLAC_ENABLE_OGG OFF CACHE BOOL "" FORCE)
set(USE_TLS OFF CACHE BOOL "" FORCE)
set(USE_ZLIB OFF CACHE BOOL "" FORCE)
set(IXWEBSOCKET_INSTALL OFF CACHE BOOL "" FORCE)

add_subdirectory(${_src}/sendspin-cpp sendspin)

# noise_pattern_expand() passes memchr() a length beyond its static pattern arrays (the end token
# is always found inside): bionic's FORTIFY (the NDK default) aborts on the length alone.
set_source_files_properties(${_src}/noise-c/src/protocol/patterns.c TARGET_DIRECTORY noise_c
  PROPERTIES COMPILE_OPTIONS -U_FORTIFY_SOURCE)
