package com.adroited.aiterm

import android.app.Application
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner

/** Process-wide pairing dependencies and the foreground/background lock clock. */
class AitermApplication : Application(), DefaultLifecycleObserver {

    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super<Application>.onCreate()
        container = AppContainer(this)
        ProcessLifecycleOwner.get().lifecycle.addObserver(this)
    }

    override fun onStart(owner: LifecycleOwner) {
        container.appLock.onEnterForeground()
    }

    override fun onStop(owner: LifecycleOwner) {
        container.appLock.onEnterBackground()
    }
}
