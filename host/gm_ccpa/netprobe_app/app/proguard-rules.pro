# R8/ProGuard keep-rules for netprobe.
#
# The Rust JNI core (native/carplay-jni/src/lib.rs) resolves these callback methods by STRING NAME
# via jni::call_method ("copyCertificate", "createSignature"). R8 renaming/stripping them breaks the
# MFi relay at runtime with no compile-time signal. Keep the whole callback surface.
-keep class zeno.gmccpa.pair.NativeCore { *; }
-keep interface zeno.gmccpa.**MfiRelay* { *; }
-keep class * implements zeno.gmccpa.**MfiRelay* { *; }

# JNI-registered native methods and any class holding an @Keep/native entry point.
-keepclasseswithmembernames class * {
    native <methods>;
}
