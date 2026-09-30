QT += core gui quick printsupport dbus network
CONFIG += c++17 console
CONFIG -= app_bundle
TARGET = backend_test
TEMPLATE = app

SOURCES += backend_test.cpp \
    ../src/cli.cpp \
    ../src/notesbackend.cpp \
    ../src/singleinstance.cpp \
    ../src/markdownhighlighter.cpp
HEADERS += ../src/cli.h ../src/vaultlock.h ../src/notesbackend.h ../src/singleinstance.h ../src/markdownhighlighter.h fake_session.h
INCLUDEPATH += ../src
