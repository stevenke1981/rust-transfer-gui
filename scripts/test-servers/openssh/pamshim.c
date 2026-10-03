/* Minimal fake libpam for running a non-root OpenSSH sshd in tests.
 * pam_authenticate asks one echo-off "Password: " question through the
 * conversation function and compares it with $SHIM_PASSWORD (default s3cret). */
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
struct pam_message { int msg_style; const char *msg; };
struct pam_response { char *resp; int resp_retcode; };
struct pam_conv { int (*conv)(int, const struct pam_message **, struct pam_response **, void *); void *appdata_ptr; };
typedef struct { struct pam_conv conv; char *user; char *service; const void *items[16]; char *env[8]; } pam_handle_t;
#define PAM_SUCCESS 0
#define PAM_AUTH_ERR 7
#define PAM_CONV_ERR 19
int pam_start(const char *service, const char *user, const struct pam_conv *conv, pam_handle_t **ph) {
    pam_handle_t *h = calloc(1, sizeof *h);
    h->conv = *conv; h->user = user ? strdup(user) : NULL; h->service = strdup(service ? service : "sshd");
    *ph = h; return PAM_SUCCESS;
}
int pam_end(pam_handle_t *h, int st) { (void)st; if (h) { free(h->user); free(h->service); free(h); } return PAM_SUCCESS; }
int pam_set_item(pam_handle_t *h, int item, const void *v) {
    if (item == 2) { free(h->user); h->user = v ? strdup(v) : NULL; return PAM_SUCCESS; }
    if (item == 5 && v) { h->conv = *(const struct pam_conv *)v; return PAM_SUCCESS; }
    if (item >= 0 && item < 16) h->items[item] = v;
    return PAM_SUCCESS;
}
int pam_get_item(const pam_handle_t *h, int item, const void **v) {
    if (item == 2) *v = h->user; else if (item == 1) *v = h->service; else if (item == 5) *v = &h->conv;
    else *v = (item >= 0 && item < 16) ? h->items[item] : NULL;
    return PAM_SUCCESS;
}
int pam_authenticate(pam_handle_t *h, int flags) {
    (void)flags;
    const char *want = getenv("SHIM_PASSWORD"); if (!want) want = "s3cret";
    struct pam_message m = { 1 /* PAM_PROMPT_ECHO_OFF */, "Password: " };
    const struct pam_message *mp = &m;
    struct pam_response *r = NULL;
    if (h->conv.conv(1, &mp, &r, h->conv.appdata_ptr) != PAM_SUCCESS || !r) return PAM_CONV_ERR;
    int ok = r[0].resp && strcmp(r[0].resp, want) == 0;
    if (r[0].resp) { memset(r[0].resp, 0, strlen(r[0].resp)); free(r[0].resp); }
    free(r);
    fprintf(stderr, "pamshim: authentication %s for %s\n", ok ? "OK" : "FAILED", h->user ? h->user : "?");
    return ok ? PAM_SUCCESS : PAM_AUTH_ERR;
}
int pam_acct_mgmt(pam_handle_t *h, int f) { (void)h; (void)f; return PAM_SUCCESS; }
int pam_setcred(pam_handle_t *h, int f) { (void)h; (void)f; return PAM_SUCCESS; }
int pam_open_session(pam_handle_t *h, int f) { (void)h; (void)f; return PAM_SUCCESS; }
int pam_close_session(pam_handle_t *h, int f) { (void)h; (void)f; return PAM_SUCCESS; }
int pam_chauthtok(pam_handle_t *h, int f) { (void)h; (void)f; return PAM_SUCCESS; }
const char *pam_strerror(pam_handle_t *h, int e) { (void)h; return e == 0 ? "Success" : "Authentication failure"; }
int pam_putenv(pam_handle_t *h, const char *s) { (void)h; (void)s; return PAM_SUCCESS; }
char **pam_getenvlist(pam_handle_t *h) { (void)h; return calloc(1, sizeof(char *)); }
const char *pam_getenv(pam_handle_t *h, const char *n) { (void)h; (void)n; return NULL; }
