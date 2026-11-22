# kotlinx.serialization keeps what it needs through its own rules; Credentials
# is the one class it encodes.
-keep,includedescriptorclasses class com.arakun.kit.Credentials { *; }
-keep class com.arakun.kit.Credentials$$serializer { *; }

# Crash reports read without a mapping file: shrink, but keep the names and
# line numbers in stack traces.
-dontobfuscate
-keepattributes SourceFile,LineNumberTable
