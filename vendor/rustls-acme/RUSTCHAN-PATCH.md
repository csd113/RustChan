# Rustls ACME date and time compatibility patch

The runtime and examples come from the crates.io rustls-acme 0.15.4 package
with SHA-256 `64f24daec4ed0dfe6327bcb2b8045bf1d1cb4835e4f824d4cf487140f4b0cee4`.

The manifest replaces Chrono with Time. Certificate validity timestamps
come directly from the existing X.509 parser. The renewal calculation
still waits until one-third of the validity period remains and schedules
immediate renewal when that time has passed. ACME modes and TLS parameters
are unchanged. Remove the patch when a compatible upstream release no
longer needs Chrono.
