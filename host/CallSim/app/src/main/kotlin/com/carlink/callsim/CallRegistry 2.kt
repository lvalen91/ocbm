package com.carlink.callsim

/** The one live call (CallSim never holds more than one). */
object CallRegistry {
    @Volatile var current: CallSimConnection? = null
}
