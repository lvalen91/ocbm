package com.carlink.callsim

import android.content.ComponentName
import android.content.Context
import android.os.Bundle
import android.telecom.PhoneAccount
import android.telecom.PhoneAccountHandle
import android.telecom.TelecomManager

object Accounts {
    private const val ACCOUNT_ID = "callsim"

    // PhoneAccount.EXTRA_ADD_SELF_MANAGED_CALLS_TO_INCALLSERVICE (API 33). String literal so
    // minSdk 31 compiles; on 31/32 Telecom just ignores the unknown key. Without this the
    // default dialer's InCallService never sees self-managed calls; Android Auto's
    // InCallService opts in on its own via METADATA_INCLUDE_SELF_MANAGED_CALLS.
    private const val EXTRA_ADD_TO_INCALLSERVICE = "android.telecom.extra.ADD_SELF_MANAGED_CALLS_TO_INCALLSERVICE"

    fun handle(ctx: Context): PhoneAccountHandle =
        PhoneAccountHandle(ComponentName(ctx, CallSimConnectionService::class.java), ACCOUNT_ID)

    fun register(ctx: Context): PhoneAccountHandle {
        val tm = ctx.getSystemService(TelecomManager::class.java)
        val h = handle(ctx)
        val extras = Bundle().apply {
            putBoolean(PhoneAccount.EXTRA_LOG_SELF_MANAGED_CALLS, true)
            putBoolean(EXTRA_ADD_TO_INCALLSERVICE, true)
        }
        val account = PhoneAccount.builder(h, "CallSim")
            .setCapabilities(PhoneAccount.CAPABILITY_SELF_MANAGED)
            .addSupportedUriScheme(PhoneAccount.SCHEME_TEL)
            .setShortDescription("CallSim fake-call test account")
            .setExtras(extras)
            .build()
        try {
            tm.registerPhoneAccount(account)
            val back = tm.getPhoneAccount(h)
            L.i("PhoneAccount registered id=$ACCOUNT_ID selfManaged=${back?.hasCapabilities(PhoneAccount.CAPABILITY_SELF_MANAGED)} enabled=${back?.isEnabled}")
        } catch (e: Exception) {
            L.e("registerPhoneAccount failed", e)
        }
        return h
    }
}
