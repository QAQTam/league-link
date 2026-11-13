/*
 * league-link — C ABI component (Windows DLL)
 *
 * Local League of Legends Client (LCU) API access from any language.
 * This header ships with the signed release alongside league_link.dll.
 *
 * Contract:
 *   - Fallible functions return int32_t: LL_OK (0) or a negative LL_ERR_*.
 *   - Call ll_last_error_message() for a human-readable detail string.
 *   - Output strings are written into caller-provided buffers. If the buffer
 *     is too small the call returns LL_ERR_BUFFER_TOO_SMALL and stores the
 *     required size (including the NUL) in *out_required.
 *   - No callbacks cross the boundary; events are polled via LlWatch.
 *
 * SPDX-License-Identifier: MIT
 */
#ifndef LEAGUE_LINK_H
#define LEAGUE_LINK_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Status codes ─────────────────────────────────────────── */
#define LL_OK                    0
#define LL_ERR_NOT_RUNNING      (-1)
#define LL_ERR_AUTH_TIMEOUT     (-2)
#define LL_ERR_HTTP             (-3)
#define LL_ERR_STATUS           (-4)
#define LL_ERR_WEBSOCKET        (-5)
#define LL_ERR_TLS              (-6)
#define LL_ERR_INVALID_HEADER   (-7)
#define LL_ERR_JSON             (-8)
#define LL_ERR_IO               (-9)
#define LL_ERR_LOCKFILE_PARSE   (-10)
#define LL_ERR_LOG_PARSE        (-11)
#define LL_ERR_BUFFER_TOO_SMALL (-12)
#define LL_ERR_INVALID_ARG      (-13)
#define LL_ERR_INTERNAL         (-14)
#define LL_ERR_NO_EVENT         (-15)
#define LL_ERR_CLOSED           (-16)

/* ── Types ────────────────────────────────────────────────── */
#define LL_PASSWORD_CAP 512

typedef struct LlCredentials {
    uint16_t port;
    uint32_t pid;
    char     password[LL_PASSWORD_CAP];
} LlCredentials;

typedef struct LlWatch LlWatch; /* opaque */

/* ── Discovery ────────────────────────────────────────────── */
int32_t ll_discover(LlCredentials* out);
int32_t ll_discover_wait(LlCredentials* out, uint64_t timeout_secs);

/* ── HTTP ─────────────────────────────────────────────────── */
int32_t ll_get(uint16_t port, const char* password, const char* path,
               char* out_json, size_t out_cap, size_t* out_required);
int32_t ll_post(uint16_t port, const char* password, const char* path,
                const char* body_json,
                char* out_json, size_t out_cap, size_t* out_required);
int32_t ll_delete(uint16_t port, const char* password, const char* path,
                  char* out_json, size_t out_cap, size_t* out_required);

/* ── Events ───────────────────────────────────────────────── */
LlWatch* ll_watch_open(uint16_t port, const char* password, const char* uri_filter);
int32_t  ll_watch_poll(LlWatch* watch, char* out_json, size_t out_cap, size_t* out_required);
void     ll_watch_close(LlWatch* watch);

/* ── Meta ─────────────────────────────────────────────────── */
const char* ll_last_error_message(void);
const char* ll_version(void);
uint32_t    ll_abi_version(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* LEAGUE_LINK_H */
