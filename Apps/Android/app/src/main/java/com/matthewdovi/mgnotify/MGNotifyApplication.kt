package com.matthewdovi.mgnotify

import android.app.Application

class MGNotifyApplication : Application() {
    lateinit var model: MGNotifyModel
        private set

    override fun onCreate() {
        super.onCreate()
        model = MGNotifyModel(this)
    }
}

val android.content.Context.mgNotifyModel: MGNotifyModel
    get() = (applicationContext as MGNotifyApplication).model
