#version 450
#extension GL_GOOGLE_include_directive : require
#include "common.glsl"

layout(set = 0, binding = 0) uniform sampler2D atlas;

layout(location = 0) flat in uint quad_index;
layout(location = 1) in vec2 local;

layout(location = 0) out vec4 color;

const uint TOP_LEFT = 1u;
const uint TOP_RIGHT = 2u;
const uint BOTTOM_RIGHT = 4u;
const uint BOTTOM_LEFT = 8u;

// Signed distance from `p`, relative to the centre, to the edge of a box reaching `half`
// each way with corners of radius `radius`.
float rounded_box(vec2 p, vec2 half_size, float radius) {
    vec2 q = abs(p) - half_size + radius;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}

void main() {
    Quad quad = quads[quad_index];
    vec4 tint = vec4(
        (quad.color >> 24) & 0xffu, (quad.color >> 16) & 0xffu,
        (quad.color >> 8) & 0xffu, quad.color & 0xffu
    ) / 255.0;
    tint.rgb *= tint.a;
    if (quad.glyph != 0u) {
        Glyph glyph = glyphs[quad.glyph - 1u];
        ivec2 texel = (halves(glyph.position) & 0xffff) + ivec2(local);
        vec4 sampled = texelFetch(atlas, texel, 0);
        color = (glyph.flags & COLOR_GLYPH) != 0u ? sampled * tint.a : tint * sampled.a;
        return;
    }
    vec2 half_size = vec2(quad.width, quad.height) / 2.0;
    vec2 p = local - half_size;
    uint corner = p.x < 0.0 ? (p.y < 0.0 ? TOP_LEFT : BOTTOM_LEFT)
                            : (p.y < 0.0 ? TOP_RIGHT : BOTTOM_RIGHT);
    float radius = (quad.corners & corner) != 0u ? quad.radius : 0.0;
    float distance = rounded_box(p, half_size, radius);
    float coverage = quad.blur > 0.0
        ? 1.0 - smoothstep(-quad.blur, quad.blur, distance)
        : clamp(0.5 - distance, 0.0, 1.0);
    color = tint * coverage;
}
