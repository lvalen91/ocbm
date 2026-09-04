package com.carlink.callsim

import android.telecom.Connection
import android.telecom.ConnectionRequest
import android.telecom.ConnectionService
import android.telecom.PhoneAccountHandle

class CallSimConnectionService : ConnectionService() {

    override fun onCreate() {
        super.onCreate()
        L.i("ConnectionService created (bound by Telecom)")
    }

    override fun onDestroy() {
        L.i("ConnectionService destroyed (Telecom unbound)")
        super.onDestroy()
    }

    override fun onCreateIncomingConnection(account: PhoneAccountHandle?, request: ConnectionRequest): Connection {
        L.i("onCreateIncomingConnection address=${request.address} extras=${request.extras?.keySet()}")
        val c = CallSimConnection(applicationContext, request, incoming = true)
        c.setRinging()
        return c
    }

    override fun onCreateOutgoingConnection(account: PhoneAccountHandle?, request: ConnectionRequest): Connection {
        L.i("onCreateOutgoingConnection address=${request.address} extras=${request.extras?.keySet()}")
        val c = CallSimConnection(applicationContext, request, incoming = false)
        c.setDialing()
        c.scheduleAutoAnswer()
        return c
    }

    override fun onCreateIncomingConnectionFailed(account: PhoneAccountHandle?, request: ConnectionRequest) {
        L.e("onCreateIncomingConnectionFailed address=${request.address} — Telecom refused the call " +
            "(another call in progress, DND with call restriction, or isIncomingCallPermitted()==false)")
        CallFgService.stop(applicationContext)
    }

    override fun onCreateOutgoingConnectionFailed(account: PhoneAccountHandle?, request: ConnectionRequest) {
        L.e("onCreateOutgoingConnectionFailed address=${request.address} — Telecom refused the call " +
            "(another call in progress or isOutgoingCallPermitted()==false)")
        CallFgService.stop(applicationContext)
    }
}
