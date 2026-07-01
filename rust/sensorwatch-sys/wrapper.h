/*
 * bindgen input for sensorwatch-sys.
 *
 * A single include of the public C ABI header keeps the generated bindings
 * (src/bindings.rs) scoped to exactly the sw_* / SW_* surface (see the allowlist
 * in regen-bindings.sh). This file is only ever read by bindgen during
 * regeneration; it is not part of the crate build.
 */
#include "sensorwatch/sensorwatch.h"
