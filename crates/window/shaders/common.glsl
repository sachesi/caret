// What the stages of both pipelines share: the cells, the frame's quads, the atlas
// entries and the constants each draw is given.

struct Cell {
    uint glyph;
    uint fg;
    uint bg;
    uint underline;
    uint flags;
};

struct Glyph {
    uint position;
    uint size;
    uint offset;
    uint flags;
};

// A rectangle of the window's frame: see chrome.rs.
struct Quad {
    float x;
    float y;
    float width;
    float height;
    uint color;
    float radius;
    uint corners;
    uint glyph;
    float blur;
    float cut[4];
    float cut_radius;
};

layout(std430, set = 0, binding = 1) readonly buffer Glyphs { Glyph glyphs[]; };
layout(std430, set = 0, binding = 2) readonly buffer Cells { Cell cells[]; };
layout(std430, set = 0, binding = 3) readonly buffer Quads { Quad quads[]; };

layout(push_constant) uniform Constants {
    float viewport_width;
    float viewport_height;
    int cell_width;
    int cell_height;
    int origin_x;
    int origin_y;
    uint columns;
    // 0 draws backgrounds, lines and the cursor; 1 draws glyphs.
    uint pass;
    int cursor_column;
    int cursor_line;
    // CursorShape, plus 16 when the cursor covers two cells and 32 when the window has
    // the focus.
    uint cursor_shape;
    uint cursor_color;
    uint background;
    uint selection;
    int baseline;
    int underline_top;
    int underline_thickness;
    int strikeout_top;
    int strikeout_thickness;
    // The pane's first cell in the cells, which its draw's instances start at.
    uint first;
} k;

const uint HAS_BACKGROUND = 1u << 16;
const uint SELECTED = 1u << 17;
const uint COLOR_GLYPH = 1u;

vec3 rgb(uint packed) {
    return vec3((packed >> 16) & 0xffu, (packed >> 8) & 0xffu, packed & 0xffu) / 255.0;
}

ivec2 halves(uint packed) {
    return ivec2(int(packed << 16) >> 16, int(packed) >> 16);
}

bool under_cursor(uint index) {
    if (k.cursor_column < 0) {
        return false;
    }
    int column = int(index % k.columns);
    int line = int(index / k.columns);
    bool wide = (k.cursor_shape & 16u) != 0u;
    return line == k.cursor_line
        && (column == k.cursor_column || (wide && column == k.cursor_column + 1));
}

// A block cursor in the focused window inverts its cell.
bool block_cursor(uint index) {
    return under_cursor(index) && (k.cursor_shape & 15u) == 0u && (k.cursor_shape & 32u) != 0u;
}
