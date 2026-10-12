//! Builds the pinned libghostty-vt as static archives for femux-vt.
//! Invoked by ../build.rs; the ghostty revision is pinned in build.zig.zon.
const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    const dep = b.dependency("ghostty", .{
        .target = target,
        .optimize = optimize,
    });
    const lib = dep.artifact("ghostty-vt-static");
    // On macOS, libSystem provides memset & co.; a bundled compiler_rt
    // duplicates them and is a misaligned archive member for Apple's ld.
    if (target.result.os.tag == .macos) lib.bundle_compiler_rt = false;
    b.installArtifact(lib);

    // As a dependency, ghostty links its vendored SIMD libraries (simdutf,
    // highway) as separate static libraries rather than one fat archive.
    // Install each of them too; build.rs links every archive in lib/.
    for (lib.root_module.getGraph().modules) |m| {
        for (m.link_objects.items) |obj| switch (obj) {
            .other_step => |c| if (c.kind == .lib) {
                if (target.result.os.tag == .macos) c.bundle_compiler_rt = false;
                b.installArtifact(c);
            },
            else => {},
        };
    }

    b.installDirectory(.{
        .source_dir = dep.path("include"),
        .install_dir = .header,
        .install_subdir = "",
    });
}
