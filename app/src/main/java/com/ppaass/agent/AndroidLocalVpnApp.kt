package com.ppaass.agent

import android.app.Application
import com.ppaass.agent.BuildConfig
import timber.log.Timber
import timber.log.Timber.DebugTree

class AndroidLocalVpnApp : Application() {

    override fun onCreate() {
        super.onCreate()

        if (BuildConfig.DEBUG) {
            Timber.plant(DebugTree())
        }
    }
}