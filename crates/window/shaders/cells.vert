#version 450
#extension GL_GOOGLE_include_directive : require
#include "common.glsl"

layout(location = 0) flat out uint cell_index;
layout(location = 1) out vec2 local;
layout(location = 2) out vec2 texel;

const vec2 corners[6] = vec2[](
    vec2(0, 0), vec2(1, 0), vec2(0, 1),
    vec2(0, 1), vec2(1, 0), vec2(1, 1)
);

void main() {
    uint index = gl_InstanceIndex - k.first;
    vec2 corner = corners[gl_VertexIndex];
    ivec2 cell_origin = ivec2(k.origin_x, k.origin_y)
        + ivec2(int(index % k.columns) * k.cell_width, int(index / k.columns) * k.cell_height);
    vec2 position;
    if (k.pass == 0u) {
        // Most cells draw nothing in this pass: their background is the terminal's, under
        // them all. Each would otherwise shade every pixel it covers to no effect.
        if (cells[gl_InstanceIndex].flags == 0u && !under_cursor(index)) {
            gl_Position = vec4(2.0, 2.0, 0.0, 1.0);
            cell_index = index;
            local = vec2(0);
            texel = vec2(0);
            return;
        }
        local = corner * vec2(k.cell_width, k.cell_height);
        position = vec2(cell_origin) + local;
        texel = vec2(0);
    } else {
        Cell cell = cells[gl_InstanceIndex];
        if (cell.glyph == 0u) {
            gl_Position = vec4(2.0, 2.0, 0.0, 1.0);
            cell_index = index;
            local = vec2(0);
            texel = vec2(0);
            return;
        }
        Glyph glyph = glyphs[cell.glyph - 1u];
        vec2 size = vec2(halves(glyph.size) & 0xffff);
        local = corner * size;
        position = vec2(cell_origin + halves(glyph.offset)) + local;
        texel = vec2(halves(glyph.position) & 0xffff) + local;
    }
    cell_index = index;
    vec2 viewport = vec2(k.viewport_width, k.viewport_height);
    gl_Position = vec4(position / viewport * 2.0 - 1.0, 0.0, 1.0);
}
