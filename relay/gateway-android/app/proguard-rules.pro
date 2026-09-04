# kotlinx.serialization keeps generated serializers through reflection lookups.
-keepclassmembers class **$$serializer { *; }
-keepclasseswithmembers class ** {
    public static ** Companion;
}
