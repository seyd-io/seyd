# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
# Build the C examples against libseyd.
#
#   make            build both examples (debug library)
#   make check      build and run the ABI conformance test
#   make PROFILE=release
#
# libseyd comes from `cargo build -p seyd-ffi`; this Makefile builds it for you.
# Linux, macOS and Windows (GNU make + MinGW gcc, e.g. an MSYS2 shell; the
# library itself may come from either the msvc or the gnu Rust toolchain).

PROFILE ?= debug
ROOT    := $(abspath $(CURDIR)/../..)
TARGET  := $(ROOT)/target/$(PROFILE)
CARGO   := cargo $(if $(filter release,$(PROFILE)),build --release,build)

CFLAGS  += -std=c11 -Wall -Wextra -O2 -I include

# The Rust cdylib pulls in the platform's TLS, threading and network libraries.
UNAME := $(shell uname -s)
EXE   :=
ifeq ($(OS),Windows_NT)
    # The DLL by path, not -lseyd: cargo also emits the static seyd.lib (MSVC
    # objects), and MinGW ld would pick that up first and fail on its /GS
    # cookie symbols. ld links a DLL given as an input file directly.
    EXE     := .exe
    LDFLAGS += $(TARGET)/seyd.dll
    RUNPATH := PATH="$(TARGET):$$PATH"
else ifeq ($(UNAME),Darwin)
    LDFLAGS += -L $(TARGET) -lseyd -framework CoreFoundation -framework Security -framework SystemConfiguration
    RUNPATH := DYLD_LIBRARY_PATH=$(TARGET)
else
    LDFLAGS += -L $(TARGET) -lseyd -lpthread -ldl -lm
    RUNPATH := LD_LIBRARY_PATH=$(TARGET)
endif

EXAMPLES := abi-smoke sensor-robot
BINS     := $(addsuffix $(EXE),$(addprefix build/,$(EXAMPLES)))

all: $(BINS)

# Always delegated to cargo, which is the thing that knows whether the library
# is stale. Order-only below, so a fresh library does not force a relink.
lib:
	cd $(ROOT) && $(CARGO) -p seyd-ffi

build/%$(EXE): examples/%.c include/seyd.h | lib
	@mkdir -p build
	$(CC) $(CFLAGS) $< -o $@ $(LDFLAGS)

check: build/abi-smoke$(EXE)
	$(RUNPATH) ./build/abi-smoke$(EXE)

clean:
	rm -rf build

.PHONY: all lib check clean
