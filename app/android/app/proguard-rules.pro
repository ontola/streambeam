# ML Kit discovers these through manifest metadata and reflection. AGP 9/R8
# strips their no-argument constructors with firebase-components 16.1.0's
# class-only keep rule, so BarcodeScanning.getClient() receives no component.
-keep class * implements com.google.firebase.components.ComponentRegistrar {
    public <init>();
}
