// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
/* sensor-robot.c — a complete Seyd robot in C, in one file.
 *
 * Announces itself to the signal cloud, publishes a counter on a sensor
 * channel at 10 Hz, and prints whatever a pilot sends on the command channel.
 * It is the C twin of sim/sensor-source.py, and the smallest honest answer to
 * "what does integrating Seyd look like?".
 *
 *   ./sensor-robot <robot_id> <signal_url> [credential_path]
 *
 * Add video by calling seyd_push_frame with encoded access units from
 * whatever encoder the robot already has — Seyd never decodes or re-encodes.
 */

#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "seyd.h"

static volatile atomic_bool running = true;

static void on_sigint(int _sig) {
    (void)_sig;
    atomic_store(&running, false);
}

/* Callbacks arrive on one Seyd thread and must not block: print and return. */

static void on_session_started(void *user, const char *session_id,
                               seyd_role role, const char *path_label) {
    (void)user;
    printf("session %s started (%s over %s)\n", session_id,
           role == SEYD_ROLE_DRIVER ? "driver" : "observer", path_label);
}

static void on_session_ended(void *user, const char *session_id,
                             const char *reason) {
    (void)user;
    /* Park actuators here — this fires whether the pilot said goodbye or
     * simply vanished. */
    printf("session %s ended (%s)\n", session_id, reason);
}

static void on_command(void *user, uint8_t channel, const uint8_t *data,
                       size_t len) {
    (void)user;
    printf("command on channel %u: %.*s\n", channel, (int)len, (const char *)data);
}

static void on_requested_config(void *user, const char *json) {
    (void)user;
    /* A real robot reconfigures its encoder here. */
    printf("requested config: %s\n", json);
}

static void on_signal_state(void *user, seyd_signal_state state,
                            const char *detail) {
    (void)user;
    const char *name = state == SEYD_SIGNAL_CONNECTED      ? "connected"
                       : state == SEYD_SIGNAL_DISCONNECTED ? "disconnected"
                                                           : "denied";
    printf("signal %s%s%s\n", name, detail ? ": " : "", detail ? detail : "");
}

static uint64_t now_us(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (uint64_t)ts.tv_sec * 1000000u + (uint64_t)ts.tv_nsec / 1000u;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr,
                "usage: %s <robot_id> <signal_url> [credential_path]\n",
                argv[0]);
        return 2;
    }
    signal(SIGINT, on_sigint);
    seyd_init_logging("info");

    seyd_config cfg = {
        .robot_id = argv[1],
        .signal_url = argv[2],
        .credential_path = argc > 3 ? argv[3] : "./robot.key",
        .quic_port = 4433,
        .ipv6 = true,
        .port_mapping = true,
        .qos_profile = "latency",
        .max_sessions = 4,
    };
    seyd_callbacks cbs = {
        .on_session_started = on_session_started,
        .on_session_ended = on_session_ended,
        .on_command = on_command,
        .on_requested_config = on_requested_config,
        .on_signal_state = on_signal_state,
    };

    seyd_agent *agent = NULL;
    if (seyd_agent_create(&cfg, &cbs, &agent) != SEYD_OK) {
        fprintf(stderr, "create: %s\n", seyd_last_error());
        return 1;
    }

    uint8_t telemetry = 0, drive = 0;
    seyd_channel_config sensor = {.kind = SEYD_CHANNEL_SENSOR,
                                  .name = "telemetry"};
    seyd_channel_config command = {.kind = SEYD_CHANNEL_COMMAND, .name = "drive"};
    if (seyd_channel_add(agent, &sensor, &telemetry) != SEYD_OK ||
        seyd_channel_add(agent, &command, &drive) != SEYD_OK) {
        fprintf(stderr, "channel_add: %s\n", seyd_last_error());
        seyd_agent_destroy(agent);
        return 1;
    }

    /* Blocks while candidates are discovered and the robot is announced. */
    if (seyd_agent_start(agent) != SEYD_OK) {
        fprintf(stderr, "start: %s\n", seyd_last_error());
        seyd_agent_destroy(agent);
        return 1;
    }
    printf("robot %s online; pilots may connect\n", cfg.robot_id);

    for (uint64_t tick = 0; atomic_load(&running); tick++) {
        char json[128];
        int n = snprintf(json, sizeof json,
                         "{\"seq\":%llu,\"t_us\":%llu}",
                         (unsigned long long)tick, (unsigned long long)now_us());
        seyd_push_message(agent, telemetry, (const uint8_t *)json, (size_t)n);
        struct timespec hz10 = {.tv_sec = 0, .tv_nsec = 100 * 1000 * 1000};
        nanosleep(&hz10, NULL);
    }

    printf("\nstopping\n");
    seyd_counters_t c = {0};
    if (seyd_counters(agent, &c) == SEYD_OK) {
        printf("sent %llu chunks, %llu bytes\n", (unsigned long long)c.chunks_sent,
               (unsigned long long)c.bytes_sent);
    }
    seyd_agent_destroy(agent);
    return 0;
}
