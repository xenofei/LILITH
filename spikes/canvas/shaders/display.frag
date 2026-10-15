#version 440
// Shows the projection (IMG-032): the mip levels are separate textures, so this shader picks the
// two levels around the wanted LOD itself and filters between them (bilinear within a level,
// sampled with textureLod since each texture has one level;
// linear across levels). The projection is premultiplied; it is composited over a grey checker.
layout(location = 0) in vec2 v_uv;
layout(location = 0) out vec4 fragColor;

layout(std140, binding = 0) uniform View {
    vec4 viewport;  // xy = item size in pixels, z = zoom (pixels per document pixel), w = bound levels
    vec4 pan;       // xy = document position of the item's top-left pixel, zw = document size
};
layout(binding = 1) uniform sampler2D level0;
layout(binding = 2) uniform sampler2D level1;
layout(binding = 3) uniform sampler2D level2;
layout(binding = 4) uniform sampler2D level3;
layout(binding = 5) uniform sampler2D level4;
layout(binding = 6) uniform sampler2D level5;
layout(binding = 7) uniform sampler2D level6;
layout(binding = 8) uniform sampler2D level7;

vec4 sampleLevel(int l, vec2 uv)
{
    if (l <= 0) return textureLod(level0, uv, 0.0);
    if (l == 1) return textureLod(level1, uv, 0.0);
    if (l == 2) return textureLod(level2, uv, 0.0);
    if (l == 3) return textureLod(level3, uv, 0.0);
    if (l == 4) return textureLod(level4, uv, 0.0);
    if (l == 5) return textureLod(level5, uv, 0.0);
    if (l == 6) return textureLod(level6, uv, 0.0);
    return textureLod(level7, uv, 0.0);
}

void main()
{
    vec2 screen = v_uv * viewport.xy;
    vec2 docPos = pan.xy + screen / viewport.z;
    vec2 cell = floor(screen / 16.0);
    float checker = mod(cell.x + cell.y, 2.0) < 1.0 ? 0.32 : 0.38;
    vec4 bg = vec4(vec3(checker), 1.0);
    if (docPos.x < 0.0 || docPos.y < 0.0 || docPos.x >= pan.z || docPos.y >= pan.w) {
        fragColor = vec4(vec3(0.18), 1.0);
        return;
    }
    float maxLevel = viewport.w - 1.0;
    float lod = clamp(log2(1.0 / viewport.z), 0.0, maxLevel);
    float l0 = floor(lod);
    float t = lod - l0;
    vec2 uv = docPos / pan.zw;
    vec4 c0 = sampleLevel(int(l0), uv);
    vec4 c1 = sampleLevel(int(min(l0 + 1.0, maxLevel)), uv);
    vec4 c = mix(c0, c1, t);
    fragColor = c + bg * (1.0 - c.a);
}
