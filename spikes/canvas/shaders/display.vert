#version 440
// Full-viewport triangle from gl_VertexIndex (no vertex buffer). v_uv: (0,0) = top-left.
layout(location = 0) out vec2 v_uv;

void main()
{
    vec2 pos = vec2(float((gl_VertexIndex << 1) & 2), float(gl_VertexIndex & 2));
    v_uv = vec2(pos.x, 1.0 - pos.y);
    gl_Position = vec4(pos * 2.0 - 1.0, 0.0, 1.0);
}
