# Build the C examples against libseyd.
#
#   make            build both examples (debug library)
#   make check      build and run the ABI conformance test
#   make PROFILE=release
#
# libseyd comes from `cargo build -p seyd-ffi`; this Makefile builds it for you.

PROFILE ?= debug
ROOT    := $(abspath $(CURDIR)/../..)
TARGET  := $(ROOT)/target/$(PROFILE)
CARGO   := cargo $(if $(filter release,$(PROFILE)),build --release,build)

CFLAGS  += -std=c11 -Wall -Wextra -O2 -I include
LDFLAGS += -L $(TARGET) -lseyd

# The Rust cdylib pulls in the platform's TLS, threading and network libraries.
UNAME := $(shell uname -s)
ifeq ($(UNAME),Darwin)
    LDFLAGS += -framework CoreFoundation -framework Security -framework SystemConfiguration
    RUNPATH := DYLD_LIBRARY_PATH=$(TARGET)
else
    LDFLAGS += -lpthread -ldl -lm
    RUNPATH := LD_LIBRARY_PATH=$(TARGET)
endif

EXAMPLES := abi-smoke sensor-robot
BINS     := $(addprefix build/,$(EXAMPLES))

all: $(BINS)

# Always delegated to cargo, which is the thing that knows whether the library
# is stale. Order-only below, so a fresh library does not force a relink.
lib:
	cd $(ROOT) && $(CARGO) -p seyd-ffi

build/%: examples/%.c include/seyd.h | lib
	@mkdir -p build
	$(CC) $(CFLAGS) $< -o $@ $(LDFLAGS)

check: build/abi-smoke
	$(RUNPATH) ./build/abi-smoke

clean:
	rm -rf build

.PHONY: all lib check clean
