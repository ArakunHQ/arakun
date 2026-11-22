package com.arakun.android

import android.app.Application

class ArakunApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        // First, so a crash anywhere after it can be reported.
        Telemetry.start(this)
    }
}
