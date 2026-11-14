QT += core gui quick printsupport dbus
CONFIG += c++17 console
CONFIG -= app_bundle
TARGET = cli_test
TEMPLATE = app

SOURCES += cli_test.cpp \
    ../src/cli.cpp \
    ../src/notesbackend.cpp \
    ../src/markdownhighlighter.cpp
HEADERS += ../src/cli.h ../src/vaultlock.h ../src/notesbackend.h ../src/markdownhighlighter.h fake_session.h
INCLUDEPATH += ../src
# The recorded iCloud answers the real engine replays (cli_test's last part).
DEFINES += NOTES_SYNC_CASSETTES=\\\"$$PWD/../../notes-sync/tests/differential/cassettes\\\"
