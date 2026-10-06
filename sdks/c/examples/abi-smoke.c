// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
/* abi-smoke.c — exercises the Seyd C ABI without touching the network.
 *
 * This is the conformance test every form factor's build should run: it proves
 * the header matches the library, that the lifecycle states are enforced, and
 * that bad arguments are refused rather than crashing. Run with `make check`.
 */

#include <assert.h>
#include <stdio.h>
#include <string.h>

#include "seyd.h"

#define CHECK(cond)                                                            \
    do {                                                                       \
        if (!(cond)) {                                                         \
            fprintf(stderr, "FAIL %s:%d: %s (last error: %s)\n", __FILE__,      \
                    __LINE__, #cond, seyd_last_error());                       \
            return 1;                                                          \
        }                                                                      \
    } while (0)

int main(void) {
    CHECK(seyd_abi_version() == SEYD_ABI_VERSION);
    CHECK(seyd_version() != NULL && strlen(seyd_version()) > 0);

    seyd_config cfg = {0};
    seyd_agent *agent = NULL;

    /* Required fields are required. */
    CHECK(seyd_agent_create(&cfg, NULL, &agent) == SEYD_ERR_INVALID_ARG);
    CHECK(seyd_agent_create(NULL, NULL, &agent) == SEYD_ERR_INVALID_ARG);

    /* An unknown QoS profile is a configuration error, caught before any
     * network work happens. */
    cfg.robot_id = "abi-smoke";
    cfg.signal_url = "ws://127.0.0.1:1/ws";
    cfg.credential_path = "/dev/null";
    cfg.qos_profile = "no-such-profile";
    CHECK(seyd_agent_create(&cfg, NULL, &agent) == SEYD_ERR_CONFIG);

    cfg.qos_profile = "latency";
    CHECK(seyd_agent_create(&cfg, NULL, &agent) == SEYD_OK);
    CHECK(agent != NULL);

    /* Channels are numbered from 1 in the order they are added. */
    uint8_t video = 0, sensor = 0, command = 0;
    seyd_channel_config vc = {.kind = SEYD_CHANNEL_VIDEO,
                              .name = "main",
                              .codec = "avc1.42001f",
                              .fps = 25};
    seyd_channel_config sc = {.kind = SEYD_CHANNEL_SENSOR, .name = "telemetry"};
    seyd_channel_config cc = {.kind = SEYD_CHANNEL_COMMAND, .name = "ptz"};
    CHECK(seyd_channel_add(agent, &vc, &video) == SEYD_OK);
    CHECK(seyd_channel_add(agent, &sc, &sensor) == SEYD_OK);
    CHECK(seyd_channel_add(agent, &cc, &command) == SEYD_OK);
    CHECK(video == 1 && sensor == 2 && command == 3);

    /* A name may only be used once. */
    CHECK(seyd_channel_add(agent, &vc, NULL) == SEYD_ERR_INVALID_ARG);
    /* A channel needs a name. */
    seyd_channel_config anon = {.kind = SEYD_CHANNEL_SENSOR};
    CHECK(seyd_channel_add(agent, &anon, NULL) == SEYD_ERR_INVALID_ARG);

    /* Nothing may be pushed before the agent starts. */
    uint8_t byte = 0;
    /* Simulcast layers (ADR 0008): declared after the channel, before start. */
    CHECK(seyd_channel_add_layer(agent, video, "low", 0) == SEYD_OK);
    CHECK(seyd_channel_add_layer(agent, video, "high", 1800) == SEYD_OK);
    /* A name, and a bitrate no other layer already claims, are both required:
       two rungs activating together means one could never be selected. */
    CHECK(seyd_channel_add_layer(agent, video, "high", 3000) == SEYD_ERR_INVALID_ARG);
    CHECK(seyd_channel_add_layer(agent, video, "other", 1800) == SEYD_ERR_INVALID_ARG);
    CHECK(seyd_channel_add_layer(agent, video, NULL, 500) == SEYD_ERR_INVALID_ARG);
    /* Layers belong to video channels, and to channels that exist. */
    CHECK(seyd_channel_add_layer(agent, sensor, "low", 0) == SEYD_ERR_INVALID_ARG);
    CHECK(seyd_channel_add_layer(agent, 99, "low", 0) == SEYD_ERR_NOT_FOUND);

    CHECK(seyd_push_frame(agent, video, &byte, 1, true, 0) == SEYD_ERR_STATE);
    CHECK(seyd_push_frame_layer(agent, video, 1, &byte, 1, true, 0) == SEYD_ERR_STATE);
    CHECK(seyd_push_message(agent, sensor, &byte, 1) == SEYD_ERR_STATE);
    CHECK(seyd_session_count(agent, NULL) == SEYD_ERR_INVALID_ARG);

    /* Stopping a never-started agent is a no-op, and destroy is safe on NULL. */
    CHECK(seyd_agent_stop(agent) == SEYD_OK);
    seyd_agent_destroy(agent);
    seyd_agent_destroy(NULL);

    printf("abi-smoke: ok (seyd %s, ABI %u)\n", seyd_version(),
           seyd_abi_version());
    return 0;
}
