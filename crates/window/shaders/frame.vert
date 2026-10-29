#version 450
#extension GL_GOOGLE_include_directive : require
#include "common.glsl"

layout(location = 0) flat out uint quad_index;
layout(location = 1) out vec2 local;

const vec2 corners[6] = vec2[](
    vec2(0, 0), vec2(1, 0), vec2(0, 1),
    vec2(0, 1), vec2(1, 0), vec2(1, 1)
);

void main() {
    Quad quad = quads[gl_InstanceIndex];
    // A shadow fades out beyond its rectangle.
    float grow = quad.blur;
    vec2 size = vec2(quad.width, quad.height) + 2.0 * grow;
    local = corners[gl_VertexIndex] * size - grow;
    vec2 position = vec2(quad.x, quad.y) + local;
    quad_index = gl_InstanceIndex;
    vec2 viewport = vec2(k.viewport_width, k.viewport_height);
    gl_Position = vec4(position / viewport * 2.0 - 1.0, 0.0, 1.0);
}
