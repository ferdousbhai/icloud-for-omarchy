QT += core gui quick printsupport dbus
CONFIG += c++17 console
CONFIG -= app_bundle
TARGET = backend_test
TEMPLATE = app

SOURCES += backend_test.cpp \
    ../src/backgroundsync.cpp \
    ../src/notesbackend.cpp \
    ../src/markdownhighlighter.cpp
HEADERS += ../src/backgroundsync.h ../src/vaultlock.h ../src/notesbackend.h ../src/markdownhighlighter.h fake_session.h
INCLUDEPATH += ../src
