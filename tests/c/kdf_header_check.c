/*
 * Compiles against include/kdf.h and links with libkdf.a; run by
 * .github/workflows/check-header.yml. A prototype that does not match the
 * library shows up as a compile error, a link error or a wrong result here.
 */
#include <stdio.h>
#include <string.h>

#include "kdf.h"

_Static_assert(sizeof(int8_t) == 1, "int8_t");
_Static_assert(sizeof(uint8_t) == 1, "uint8_t");
_Static_assert(sizeof(int32_t) == 4, "int32_t");

static int failures;

#define CHECK(cond)                                                      \
    do {                                                                 \
        if (!(cond)) {                                                   \
            printf("FAIL line %d: %s\n", __LINE__, #cond);               \
            failures++;                                                  \
        }                                                                \
    } while (0)

static void log_line(const char *line) { (void)line; }

int main(void) {
    /* The callback type matches what the functions take. */
    kdf_log_callback cb = log_line;
    (void)cb;

    CHECK(mm2_main_status() == 0); /* not running */
    CHECK(mm2_stop() == 1);        /* not running */
    CHECK(mm2_main(NULL, log_line) == 2); /* conf is NULL */
    CHECK(mm2_main_status() == 0);

    char loopback[] = "127.0.0.1";
    char loopback6[] = "::1";
    char public_ip[] = "8.8.8.8";
    char junk[] = "not an ip";
    CHECK(is_loopback_ip(loopback) == 1);
    CHECK(is_loopback_ip(loopback6) == 1);
    CHECK(is_loopback_ip(public_ip) == 0);
    CHECK(is_loopback_ip(junk) == 0);
    CHECK(is_loopback_ip(NULL) == 0);
    CHECK(strcmp(loopback, "127.0.0.1") == 0); /* not modified */

    log_stacktrace(NULL);
    log_stacktrace("header check");

    if (failures) {
        printf("%d check(s) failed\n", failures);
        return 1;
    }
    printf("kdf.h check passed\n");
    return 0;
}
