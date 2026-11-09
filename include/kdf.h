/*
 * kdf.h -- C interface of the KDF Reloaded static library (libkdf.a).
 *
 * Link libkdf.a into the host application and call the functions below.
 * The prototypes follow the exporting source, mm2_native_lib.rs and for_c.rs;
 * CI compiles, links and runs a test program against this header and checks
 * that the symbols declared here are exactly the KDF symbols the archive
 * exports.
 *
 * Needs only <stdint.h>. License: GPL-2.0-only, like the library.
 */
#ifndef KDF_H
#define KDF_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Log callback.
 *
 * - C calling convention (the platform default; no __stdcall).
 * - Must not be NULL.
 * - `line` is a NUL-terminated string that is valid only during the call;
 *   copy it if you need it later.
 * - It is called from whichever thread logs, so it can run on any thread and
 *   must be thread-safe.
 */
typedef void (*kdf_log_callback)(const char *line);

/*
 * Supported API
 */

/*
 * Starts the daemon in a detached background thread and returns at once.
 * `conf` is the MM2.json configuration as a NUL-terminated UTF-8 JSON string.
 * `log_cb` receives the log lines.
 * Returns: 0 started, 1 already running, 2 `conf` is NULL, 3 `conf` is not
 * valid UTF-8, 5 the thread could not be started.
 */
int8_t mm2_main(const char *conf, kdf_log_callback log_cb);

/*
 * State of the daemon: 0 not running, 1 running but no context yet, 2 context
 * ready but no RPC yet, 3 RPC is up.
 */
int8_t mm2_main_status(void);

/*
 * Stops the daemon.
 * Returns: 0 stopped, 1 not running, 2 error while stopping, 3 already
 * stopping.
 */
int8_t mm2_stop(void);

/*
 * Runs the built-in self tests. If the daemon is running, it is stopped first
 * and restarted with the same configuration after a successful run. Returns
 * `torch` on success and -1 on any failure: a test failed, a run is already
 * in progress, or stopping or restarting the daemon failed. After a failure
 * mm2_test does not restore the daemon's state: it may be stopped, or
 * running (stopping it failed, or the failure came during the restart);
 * mm2_main_status tells which. Further calls return -1. A -1 that only
 * reports a run in progress changes nothing in the daemon's state.
 */
int32_t mm2_test(int32_t torch, kdf_log_callback log_cb);

/*
 * Exported for internal use, not a supported API, may change without notice.
 * They are declared here only because the library exports them.
 */

/* Writes a stack trace to the log; `desc` may be NULL. */
void log_stacktrace(const char *desc);

/* Returns 1 if `ip` is a loopback address, otherwise (or on bad input) 0.
 * `ip` is not modified. */
uint8_t is_loopback_ip(char *ip);

#ifdef __cplusplus
}
#endif

#endif /* KDF_H */
