# kotlinx.serialization keeps what it needs through its own rules; AccountSession
# is the one class it encodes.
-keep,includedescriptorclasses class com.arakun.kit.AccountSession { *; }
-keep class com.arakun.kit.AccountSession$$serializer { *; }

# Crash reports read without a mapping file: shrink, but keep the names and
# line numbers in stack traces.
-dontobfuscate
-keepattributes SourceFile,LineNumberTable
