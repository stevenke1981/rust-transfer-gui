# Patched `libssh2-sys` 0.3.3

This is an unmodified copy of the `libssh2-sys` 0.3.3 crate from crates.io
(trimmed to the files its `build.rs` needs) with **one** change in
`libssh2/src/userauth.c` (`_libssh2_key_sign_algorithm`):

```diff
-    filtered_algs = LIBSSH2_ALLOC(session, strlen(supported_algs) + 1);
+    filtered_algs = LIBSSH2_ALLOC(session,
+                                  strlen(session->server_sign_algorithms) + 1);
```

The bundled libssh2 snapshot (1.11.1_DEV) sizes this buffer after the client's
supported-algorithm list but fills it from the server's `server-sig-algs`
list. A server that advertises duplicate entries (AsyncSSH does) overflows the
heap during public-key authentication with RSA keys (`free(): invalid next size`).
Upstream libssh2 already sizes the buffer after the server list; this backports that.

It is wired in through `[patch.crates-io]` in the top-level `Cargo.toml`.
Remove the patch once a fixed `libssh2-sys` is released.
