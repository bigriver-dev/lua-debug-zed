/*
 * implements a rust native lua_sethook callback
 * executed by target process lua vm/intrepeter thread on line/call events
 * intercepts breakpoints and pauses execution using m
 */
